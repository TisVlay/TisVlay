//! Wayland idle detection using ext-idle-notify-v1 protocol.
//!
//! This module provides greeter wake detection for Wayland compositors (KDE Plasma 6+,
//! Sway, Hyprland, GNOME, etc.) by using the standard ext-idle-notify-v1 protocol.
//!
//! Unlike the D-Bus GetSessionIdleTime approach (which doesn't work on KDE Plasma Wayland),
//! this directly connects to the Wayland compositor to receive idle/resumed events.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{
    globals::{registry_queue_init, GlobalList, GlobalListContents},
    protocol::wl_registry,
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

use crate::session_monitor::SessionEvent;

/// Timeout duration for idle detection in milliseconds.
/// This must be short enough that the user goes "idle" in the brief pause
/// between locking the screen (e.g., pressing Super+L) and moving the
/// mouse to wake the greeter. The ext-idle-notify-v1 protocol requires
/// the idle→resumed cycle: "idled" must fire before "resumed" can trigger
/// GreeterWake. At 200ms, even rapid lock-then-move sequences will work
/// because the natural pause between the lock keypress and mouse movement
/// exceeds this threshold.
const IDLE_TIMEOUT_MS: u32 = 200;

/// Maximum protocol version we support for ext_idle_notifier_v1.
/// The wayland-protocols crate supports up to version 1 for this protocol.
const EXT_IDLE_NOTIFIER_VERSION: u32 = 1;

/// Maximum protocol version we support for wl_seat.
const WL_SEAT_VERSION: u32 = 1;

/// State for the Wayland idle monitor.
struct WaylandState {
    /// Channel to send events to the main event handler.
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    /// Whether the screen is currently locked.
    is_locked: Arc<AtomicBool>,
    /// Whether we've already sent a GreeterWake event for this lock cycle.
    greeter_wake_sent: bool,
    /// Whether we're currently in idle state.
    is_idle: bool,
    /// The idle notification object (kept alive to receive events).
    _idle_notification: Option<ExtIdleNotificationV1>,
    /// The seat we're monitoring.
    _seat: Option<WlSeat>,
}

impl WaylandState {
    fn new(event_tx: mpsc::UnboundedSender<SessionEvent>, is_locked: Arc<AtomicBool>) -> Self {
        Self {
            event_tx,
            is_locked,
            greeter_wake_sent: false,
            is_idle: false,
            _idle_notification: None,
            _seat: None,
        }
    }

    /// Handle the idle notification becoming idle.
    fn handle_idled(&mut self) {
        debug!("Wayland idle notification: idled");
        self.is_idle = true;
    }

    /// Handle the idle notification being resumed (user activity detected).
    fn handle_resumed(&mut self) {
        debug!("Wayland idle notification: resumed");
        self.is_idle = false;

        // Only fire GreeterWake if:
        // 1. Screen is locked
        // 2. We haven't already sent the event this lock cycle
        if self.is_locked.load(Ordering::SeqCst) && !self.greeter_wake_sent {
            debug!("User activity detected while locked - sending GreeterWake");
            self.greeter_wake_sent = true;
            if self.event_tx.send(SessionEvent::GreeterWake).is_err() {
                warn!("Event channel closed");
            }
        }

        // Reset greeter_wake_sent when screen is unlocked
        if !self.is_locked.load(Ordering::SeqCst) {
            self.greeter_wake_sent = false;
        }
    }
}

// Dispatch implementation for wl_registry to handle global objects
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // Registry events are handled by GlobalListContents
    }
}

// Dispatch implementation for WlSeat (user data is version number)
impl Dispatch<WlSeat, u32> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: wayland_client::protocol::wl_seat::Event,
        _data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // We don't need to handle seat events, just need the seat object
    }
}

// Dispatch implementation for ExtIdleNotifierV1 (user data is version number)
impl Dispatch<ExtIdleNotifierV1, u32> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtIdleNotifierV1,
        _event: wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::Event,
        _data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // The notifier doesn't emit events, only the notification does
    }
}

// Dispatch implementation for ExtIdleNotificationV1
impl Dispatch<ExtIdleNotificationV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                state.handle_idled();
            }
            ext_idle_notification_v1::Event::Resumed => {
                state.handle_resumed();
            }
            _ => {}
        }
    }
}

/// Start the Wayland idle monitor in a dedicated thread.
///
/// This monitors for user activity via the ext-idle-notify-v1 protocol.
/// When the screen is locked and user activity is detected, it sends
/// a `GreeterWake` event.
///
/// Returns `Ok(())` if the monitor started successfully, or an error if
/// Wayland is not available or the protocol is not supported.
pub fn start_wayland_idle_monitor(
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    is_locked: Arc<AtomicBool>,
) -> Result<(), WaylandIdleError> {
    // Try to connect to Wayland first to validate it's available
    let conn = Connection::connect_to_env().map_err(|e| {
        WaylandIdleError::ConnectionFailed(format!("Failed to connect to Wayland: {}", e))
    })?;

    // Initialize the event queue and registry
    let (globals, event_queue): (GlobalList, EventQueue<WaylandState>) = registry_queue_init(&conn)
        .map_err(|e| {
            WaylandIdleError::ConnectionFailed(format!("Failed to initialize registry: {}", e))
        })?;

    // Get the list of available globals
    let global_list = globals.contents().clone_list();

    // Check if ext_idle_notifier_v1 is available
    let notifier_global = global_list
        .iter()
        .find(|g| g.interface == "ext_idle_notifier_v1")
        .ok_or_else(|| {
            WaylandIdleError::ProtocolNotSupported(
                "ext_idle_notifier_v1 protocol not available".to_string(),
            )
        })?
        .clone();

    // Check if wl_seat is available
    let seat_global = global_list
        .iter()
        .find(|g| g.interface == "wl_seat")
        .ok_or_else(|| WaylandIdleError::ProtocolNotSupported("wl_seat not available".to_string()))?
        .clone();

    info!(
        "Wayland idle notifier available (server: v{}, using: v{})",
        notifier_global.version,
        notifier_global.version.min(EXT_IDLE_NOTIFIER_VERSION)
    );

    // Spawn the monitor thread
    thread::Builder::new()
        .name("wayland-idle".to_string())
        .spawn(move || {
            run_wayland_idle_loop(
                conn,
                event_queue,
                globals,
                event_tx,
                is_locked,
                notifier_global,
                seat_global,
            );
        })
        .map_err(|e| WaylandIdleError::ThreadSpawnFailed(e.to_string()))?;

    Ok(())
}

/// Run the Wayland event loop in a dedicated thread.
fn run_wayland_idle_loop(
    _conn: Connection,
    mut event_queue: EventQueue<WaylandState>,
    globals: GlobalList,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    is_locked: Arc<AtomicBool>,
    notifier_global: wayland_client::globals::Global,
    seat_global: wayland_client::globals::Global,
) {
    let mut state = WaylandState::new(event_tx, is_locked);
    let qhandle = event_queue.handle();

    // Get the registry for direct binding with explicit version control.
    // We use registry.bind() instead of globals.bind() because globals.bind()
    // panics when the server advertises a version higher than the client library
    // supports (e.g., KWin advertises v20 but wayland-protocols only has v2).
    let registry = globals.registry();

    // Bind to the seat with explicit version (capped at what we support)
    let seat_version = seat_global.version.min(WL_SEAT_VERSION);
    let seat: WlSeat = registry.bind(seat_global.name, seat_version, &qhandle, seat_version);
    state._seat = Some(seat.clone());

    // Bind to the idle notifier with explicit version (capped at what we support)
    let notifier_version = notifier_global.version.min(EXT_IDLE_NOTIFIER_VERSION);
    let notifier: ExtIdleNotifierV1 = registry.bind(
        notifier_global.name,
        notifier_version,
        &qhandle,
        notifier_version,
    );

    // Create the idle notification with our timeout
    let notification = notifier.get_idle_notification(IDLE_TIMEOUT_MS, &seat, &qhandle, ());
    state._idle_notification = Some(notification);

    info!(
        "Wayland greeter wake monitor started (timeout: {}ms)",
        IDLE_TIMEOUT_MS
    );

    // Run the event loop
    loop {
        // Blocking dispatch - waits for events
        match event_queue.blocking_dispatch(&mut state) {
            Ok(_) => {
                // Reset greeter_wake_sent when screen is unlocked
                if !state.is_locked.load(Ordering::SeqCst) {
                    state.greeter_wake_sent = false;
                }
            }
            Err(e) => {
                error!("Wayland event dispatch error: {}", e);
                break;
            }
        }
    }

    warn!("Wayland idle monitor exited");
}

/// Errors that can occur when starting the Wayland idle monitor.
#[derive(Debug)]
pub enum WaylandIdleError {
    /// Failed to connect to Wayland compositor.
    ConnectionFailed(String),
    /// The ext-idle-notify-v1 protocol is not supported.
    ProtocolNotSupported(String),
    /// Failed to spawn the monitor thread.
    ThreadSpawnFailed(String),
}

impl std::fmt::Display for WaylandIdleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(msg) => write!(f, "Wayland connection failed: {}", msg),
            Self::ProtocolNotSupported(msg) => write!(f, "Protocol not supported: {}", msg),
            Self::ThreadSpawnFailed(msg) => write!(f, "Thread spawn failed: {}", msg),
        }
    }
}

impl std::error::Error for WaylandIdleError {}
