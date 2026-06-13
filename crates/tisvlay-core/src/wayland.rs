//! Wayland idle detection transport (`ext-idle-notify-v1`).
//!
//! Policy-free: connects to the session's Wayland compositor and forwards raw
//! idle/resume transitions on a channel. Consumers layer their own policy on
//! top — the session service's lock-aware greeter-wake, or the greeter idle
//! client's screen blank/restore.
//!
//! Uses the standard `ext-idle-notify-v1` protocol (KDE Plasma 6+, Sway,
//! Hyprland, Mutter, …) rather than the D-Bus `GetSessionIdleTime` approach,
//! which does not work on KDE Plasma Wayland.

use std::os::unix::net::UnixStream;
use std::thread;
use tokio::sync::mpsc;
use tracing::{error, info, trace, warn};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};

/// Maximum protocol version we support for `ext_idle_notifier_v1`.
/// The wayland-protocols crate supports up to version 1 for this protocol.
const EXT_IDLE_NOTIFIER_VERSION: u32 = 1;

/// Maximum protocol version we support for `wl_seat`.
const WL_SEAT_VERSION: u32 = 1;

/// A raw idle-state transition reported by the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleTransition {
    /// The seat went idle (no input for the registered timeout).
    Idled,
    /// Input resumed after an idle period.
    Resumed,
}

/// State for the Wayland idle monitor. Forwards transitions verbatim — no
/// lock/greeter policy lives here.
struct WaylandState {
    /// Channel to forward raw idle transitions to the consumer.
    tx: mpsc::UnboundedSender<IdleTransition>,
    /// The idle notification object (kept alive to receive events).
    _idle_notification: Option<ExtIdleNotificationV1>,
    /// The seat we're monitoring.
    _seat: Option<WlSeat>,
}

impl WaylandState {
    fn new(tx: mpsc::UnboundedSender<IdleTransition>) -> Self {
        Self {
            tx,
            _idle_notification: None,
            _seat: None,
        }
    }

    fn forward(&self, transition: IdleTransition) {
        trace!(?transition, "Wayland idle transition");
        if self.tx.send(transition).is_err() {
            warn!("Idle transition channel closed");
        }
    }
}

// Dispatch implementation for wl_registry to handle global objects.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // Registry events are handled by GlobalListContents.
    }
}

// Dispatch implementation for WlSeat (user data is version number).
impl Dispatch<WlSeat, u32> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: wayland_client::protocol::wl_seat::Event,
        _data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // We don't need seat events, just the seat object.
    }
}

// Dispatch implementation for ExtIdleNotifierV1 (user data is version number).
impl Dispatch<ExtIdleNotifierV1, u32> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ExtIdleNotifierV1,
        _event: ext_idle_notifier_v1::Event,
        _data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // The notifier doesn't emit events, only the notification does.
    }
}

// Dispatch implementation for ExtIdleNotificationV1.
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
            ext_idle_notification_v1::Event::Idled => state.forward(IdleTransition::Idled),
            ext_idle_notification_v1::Event::Resumed => state.forward(IdleTransition::Resumed),
            _ => {}
        }
    }
}

/// Start a Wayland idle watch on a dedicated thread.
///
/// Connects to the compositor named by the `WAYLAND_DISPLAY` / `XDG_RUNTIME_DIR`
/// environment and registers an `ext-idle-notify-v1` notification with the
/// given `timeout_ms`. Each `Idled` / `Resumed` edge is forwarded on `tx`. The
/// watch thread exits (closing `tx`) when the compositor disconnects, so a
/// caller can treat a closed channel as "compositor gone".
///
/// Returns an error if Wayland is unavailable or the protocol is missing.
pub fn start_idle_watch(
    timeout_ms: u32,
    tx: mpsc::UnboundedSender<IdleTransition>,
) -> Result<(), WaylandIdleError> {
    let conn = Connection::connect_to_env().map_err(|e| {
        WaylandIdleError::ConnectionFailed(format!("Failed to connect to Wayland: {}", e))
    })?;
    spawn_watch(conn, timeout_ms, tx)
}

/// Like [`start_idle_watch`], but against an already-connected compositor
/// socket rather than the `WAYLAND_DISPLAY` environment.
///
/// The greeter client uses this: its `systemd.user` unit doesn't carry
/// `WAYLAND_DISPLAY`, and the greeter compositor's socket is not necessarily
/// `wayland-0` (a lingering user manager can take that name), so the greeter
/// discovers the actual `wayland-N` socket under `XDG_RUNTIME_DIR` and connects
/// to it explicitly. `connect_to_env`'s `wayland-0` default would miss it.
pub fn start_idle_watch_on_socket(
    stream: UnixStream,
    timeout_ms: u32,
    tx: mpsc::UnboundedSender<IdleTransition>,
) -> Result<(), WaylandIdleError> {
    let conn = Connection::from_socket(stream).map_err(|e| {
        WaylandIdleError::ConnectionFailed(format!("Failed to connect to Wayland socket: {}", e))
    })?;
    spawn_watch(conn, timeout_ms, tx)
}

/// Initialize the registry on an established connection and spawn the watch
/// thread that forwards idle/resume transitions.
fn spawn_watch(
    conn: Connection,
    timeout_ms: u32,
    tx: mpsc::UnboundedSender<IdleTransition>,
) -> Result<(), WaylandIdleError> {
    // Initialize the event queue and registry.
    let (globals, event_queue): (GlobalList, EventQueue<WaylandState>) = registry_queue_init(&conn)
        .map_err(|e| {
            WaylandIdleError::ConnectionFailed(format!("Failed to initialize registry: {}", e))
        })?;

    let global_list = globals.contents().clone_list();

    let notifier_global = global_list
        .iter()
        .find(|g| g.interface == "ext_idle_notifier_v1")
        .ok_or_else(|| {
            WaylandIdleError::ProtocolNotSupported(
                "ext_idle_notifier_v1 protocol not available".to_string(),
            )
        })?
        .clone();

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

    thread::Builder::new()
        .name("wayland-idle".to_string())
        .spawn(move || {
            run_idle_loop(
                conn,
                event_queue,
                globals,
                tx,
                timeout_ms,
                notifier_global,
                seat_global,
            );
        })
        .map_err(|e| WaylandIdleError::ThreadSpawnFailed(e.to_string()))?;

    Ok(())
}

/// Run the Wayland event loop in a dedicated thread.
fn run_idle_loop(
    _conn: Connection,
    mut event_queue: EventQueue<WaylandState>,
    globals: GlobalList,
    tx: mpsc::UnboundedSender<IdleTransition>,
    timeout_ms: u32,
    notifier_global: wayland_client::globals::Global,
    seat_global: wayland_client::globals::Global,
) {
    let mut state = WaylandState::new(tx);
    let qhandle = event_queue.handle();

    // Bind via registry.bind() with explicit version control rather than
    // globals.bind(), which panics when the server advertises a higher
    // version than the client library supports (KWin advertises v20 but
    // wayland-protocols only has v1).
    let registry = globals.registry();

    let seat_version = seat_global.version.min(WL_SEAT_VERSION);
    let seat: WlSeat = registry.bind(seat_global.name, seat_version, &qhandle, seat_version);
    state._seat = Some(seat.clone());

    let notifier_version = notifier_global.version.min(EXT_IDLE_NOTIFIER_VERSION);
    let notifier: ExtIdleNotifierV1 = registry.bind(
        notifier_global.name,
        notifier_version,
        &qhandle,
        notifier_version,
    );

    let notification = notifier.get_idle_notification(timeout_ms, &seat, &qhandle, ());
    state._idle_notification = Some(notification);

    info!("Wayland idle watch started (timeout: {}ms)", timeout_ms);

    loop {
        match event_queue.blocking_dispatch(&mut state) {
            Ok(_) => {}
            Err(e) => {
                error!("Wayland event dispatch error: {}", e);
                break;
            }
        }
    }

    warn!("Wayland idle watch exited");
}

/// Errors that can occur when starting the Wayland idle watch.
#[derive(Debug)]
pub enum WaylandIdleError {
    /// Failed to connect to the Wayland compositor.
    ConnectionFailed(String),
    /// The `ext-idle-notify-v1` protocol is not supported.
    ProtocolNotSupported(String),
    /// Failed to spawn the watch thread.
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
