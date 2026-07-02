//! Wayland idle detection transport (`ext-idle-notify-v1`).
//!
//! Policy-free: connects to the session's Wayland compositor and forwards raw
//! idle/resume transitions on a channel. Consumers layer their own policy on
//! top — the session service's lock-screen wake, or the greeter idle
//! client's screen blank/restore.
//!
//! Uses the standard `ext-idle-notify-v1` protocol (KDE Plasma 6+, Sway,
//! Hyprland, Mutter, …) rather than the D-Bus `GetSessionIdleTime` approach,
//! which does not work on KDE Plasma Wayland.

use rustix::event::{PollFd, PollFlags, poll};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::thread;
use tokio::sync::mpsc;
use tracing::{error, info, trace, warn};
use wayland_client::backend::WaylandError;
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

/// Protocol version we bind for `ext_idle_notifier_v1`. v1's
/// `get_idle_notification` respects idle inhibitors, so the screen won't
/// blank (or a TV power off) mid-video — the behavior a blank/power-on-idle
/// policy wants. A newer protocol version adds only an input-only
/// `get_input_idle_notification` that ignores inhibitors, so binding higher
/// buys nothing here; the registry bind caps whatever version the compositor
/// advertises down to this constant.
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

/// One idle notification to register on a watch: a caller-assigned `tag`
/// (echoed back with every transition, so a consumer running several
/// notifications on one connection can tell them apart) and the idle
/// `timeout_ms` after which the compositor reports `Idled`.
#[derive(Debug, Clone, Copy)]
pub struct IdleNotification {
    /// Caller-assigned identifier, returned alongside each transition.
    pub tag: u32,
    /// Idle timeout in milliseconds.
    pub timeout_ms: u32,
}

/// State for the Wayland idle monitor. Forwards `(tag, transition)` verbatim —
/// no lock/idle policy lives here.
struct WaylandState {
    /// Channel to forward `(tag, transition)` to the consumer; `tag` identifies
    /// which registered notification fired.
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
    /// The idle notification objects (kept alive to receive events).
    _idle_notifications: Vec<ExtIdleNotificationV1>,
    /// The seat we're monitoring.
    _seat: Option<WlSeat>,
}

impl WaylandState {
    fn new(tx: mpsc::UnboundedSender<(u32, IdleTransition)>) -> Self {
        Self {
            tx,
            _idle_notifications: Vec::new(),
            _seat: None,
        }
    }

    fn forward(&self, tag: u32, transition: IdleTransition) {
        trace!(tag, ?transition, "Wayland idle transition");
        if self.tx.send((tag, transition)).is_err() {
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

// Dispatch implementation for ExtIdleNotificationV1 (user data is the caller's tag).
impl Dispatch<ExtIdleNotificationV1, u32> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.forward(*data, IdleTransition::Idled),
            ext_idle_notification_v1::Event::Resumed => {
                state.forward(*data, IdleTransition::Resumed)
            }
            _ => {}
        }
    }
}

/// Handle to a running idle watch. Hold it for as long as the watch should
/// run; **dropping it stops the watch** — the watch thread observes the closed
/// stop pipe (via `poll`) and exits. A consumer that needs to re-register
/// notifications (e.g. the session when its idle timeout changes) drops the old
/// handle and starts a fresh watch.
#[derive(Debug)]
pub struct WatchHandle {
    /// Write end of the stop pipe; its `Drop` (closing the fd) is the stop
    /// signal the watch thread's `poll` wakes on (`POLLHUP`). Held only for
    /// that side effect.
    #[allow(dead_code)]
    stop: OwnedFd,
}

/// Start a Wayland idle watch on a dedicated thread.
///
/// Connects to the compositor named by the `WAYLAND_DISPLAY` / `XDG_RUNTIME_DIR`
/// environment and registers one `ext-idle-notify-v1` notification per entry in
/// `notifications`. Each `Idled` / `Resumed` edge is forwarded on `tx` as
/// `(tag, transition)`, where `tag` is the entry's caller-assigned id. The watch
/// thread exits (closing `tx`) when the compositor disconnects, so a caller can
/// treat a closed channel as "compositor gone".
///
/// Returns an error if Wayland is unavailable or the protocol is missing.
pub fn start_idle_watch(
    notifications: Vec<IdleNotification>,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<WatchHandle, WaylandIdleError> {
    let conn = Connection::connect_to_env().map_err(|e| {
        WaylandIdleError::ConnectionFailed(format!("Failed to connect to Wayland: {}", e))
    })?;
    spawn_watch(conn, notifications, tx)
}

/// Like [`start_idle_watch`], but against an already-connected compositor
/// socket rather than the `WAYLAND_DISPLAY` environment.
///
/// The greeter client uses this: its `systemd.user` unit doesn't carry
/// `WAYLAND_DISPLAY`, and wayland-client's `connect_to_env` requires it (returns
/// `NoCompositor` when unset; it does not fall back to `wayland-0` the way
/// libwayland's C client does). So the greeter discovers the actual `wayland-N`
/// socket under `XDG_RUNTIME_DIR` and connects to it via this entry point.
pub fn start_idle_watch_on_socket(
    stream: UnixStream,
    notifications: Vec<IdleNotification>,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<WatchHandle, WaylandIdleError> {
    let conn = Connection::from_socket(stream).map_err(|e| {
        WaylandIdleError::ConnectionFailed(format!("Failed to connect to Wayland socket: {}", e))
    })?;
    spawn_watch(conn, notifications, tx)
}

/// Initialize the registry on an established connection and spawn the watch
/// thread that forwards idle/resume transitions.
fn spawn_watch(
    conn: Connection,
    notifications: Vec<IdleNotification>,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<WatchHandle, WaylandIdleError> {
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

    // Stop pipe: the returned `WatchHandle` holds the write end; closing it
    // (the handle dropped) wakes the watch's `poll` on the read end so it exits
    // cleanly, allowing a fresh watch to be registered.
    let (stop_read, stop_write) = rustix::pipe::pipe()
        .map_err(|e| WaylandIdleError::ThreadSpawnFailed(format!("stop pipe: {}", e)))?;

    thread::Builder::new()
        .name("wayland-idle".to_string())
        .spawn(move || {
            run_idle_loop(
                conn,
                event_queue,
                globals,
                tx,
                notifications,
                notifier_global,
                seat_global,
                stop_read,
            );
        })
        .map_err(|e| WaylandIdleError::ThreadSpawnFailed(e.to_string()))?;

    Ok(WatchHandle { stop: stop_write })
}

/// Run the Wayland event loop in a dedicated thread.
#[allow(clippy::too_many_arguments)]
fn run_idle_loop(
    _conn: Connection,
    mut event_queue: EventQueue<WaylandState>,
    globals: GlobalList,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
    notifications: Vec<IdleNotification>,
    notifier_global: wayland_client::globals::Global,
    seat_global: wayland_client::globals::Global,
    stop_read: OwnedFd,
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

    for n in &notifications {
        let notification = notifier.get_idle_notification(n.timeout_ms, &seat, &qhandle, n.tag);
        state._idle_notifications.push(notification);
    }

    info!(count = notifications.len(), "Wayland idle watch started");

    // Interruptible event loop: equivalent to repeatedly calling
    // `blocking_dispatch`, but the `poll` also watches the stop pipe so the
    // watch exits promptly when its `WatchHandle` is dropped (e.g. to
    // re-register with a new idle timeout). Mirrors wayland-backend's own
    // `blocking_read` poll, with the stop fd added as a second source.
    loop {
        if let Err(e) = event_queue.dispatch_pending(&mut state) {
            error!("Wayland event dispatch error: {}", e);
            break;
        }
        if let Err(e) = event_queue.flush() {
            error!("Wayland flush error: {}", e);
            break;
        }
        let guard = match event_queue.prepare_read() {
            Some(guard) => guard,
            // Events are already queued; loop back to dispatch them.
            None => continue,
        };
        let (stop_ready, wl_ready) = {
            let wl_fd = guard.connection_fd();
            let mut fds = [
                PollFd::new(&wl_fd, PollFlags::IN | PollFlags::ERR),
                PollFd::new(&stop_read, PollFlags::IN | PollFlags::HUP),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                // Interrupted by a signal — drop the guard and retry.
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => {
                    error!("Wayland poll error: {}", e);
                    break;
                }
            }
            (
                fds[1].revents().intersects(PollFlags::IN | PollFlags::HUP),
                fds[0].revents().intersects(PollFlags::IN | PollFlags::ERR),
            )
        };
        if stop_ready {
            info!("Wayland idle watch stopped (handle dropped)");
            break;
        }
        if wl_ready {
            match guard.read() {
                Ok(_) => {}
                // Spurious readiness — the next iteration retries.
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    error!("Wayland read error: {}", e);
                    break;
                }
            }
        } else {
            drop(guard);
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
