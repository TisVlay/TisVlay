//! logind integration for system lifecycle events.
//!
//! Monitors `org.freedesktop.login1` on the system bus for:
//! - `PrepareForShutdown`: execute `on_shutdown` or `on_reboot` action
//!   (distinguished via the `ScheduledShutdown` property)
//! - `PrepareForSleep`: execute `on_sleep` or `on_wake` action
//! - Session tracking: detect when no user sessions are active
//!
//! Takes a shutdown inhibitor lock so the daemon can gracefully
//! shut down TVs before the system powers off.

use crate::tv_manager::{SystemEvent, TvManager};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use zbus::proxy;

// =============================================================================
// logind D-Bus Proxies
// =============================================================================

/// Proxy for org.freedesktop.login1.Manager.
#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    /// Take an inhibitor lock.
    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;

    /// List current sessions.
    /// Returns: [(session_id, uid, user_name, seat_id, session_object_path)]
    fn list_sessions(
        &self,
    ) -> zbus::Result<Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)>>;

    /// The scheduled shutdown type and timestamp.
    /// Returns (type, usec) where type is "poweroff", "reboot", "halt", or "kexec".
    #[zbus(property)]
    fn scheduled_shutdown(&self) -> zbus::Result<(String, u64)>;

    /// Signal emitted before system shutdown/reboot.
    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;

    /// Signal emitted before system sleep/hibernate and after wake.
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;

    /// Signal emitted when a new session is created.
    #[zbus(signal)]
    fn session_new(
        &self,
        session_id: String,
        session_path: zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Signal emitted when a session is removed.
    #[zbus(signal)]
    fn session_removed(
        &self,
        session_id: String,
        session_path: zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

/// Proxy for org.freedesktop.login1.Session.
#[proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    /// Whether this session is the active one on its seat.
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;

    /// Session class: "user", "greeter", "lock-screen", etc.
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;

    /// Session type: "x11", "wayland", "tty", etc.
    #[zbus(property, name = "Type")]
    fn session_type(&self) -> zbus::Result<String>;
}

// =============================================================================
// LogindMonitor
// =============================================================================

/// Monitors logind for system lifecycle events and session changes.
pub struct LogindMonitor {
    manager: Arc<TvManager>,
    /// Whether to use session bus (development mode) instead of system bus.
    use_session_bus: bool,
}

impl LogindMonitor {
    pub fn new(manager: Arc<TvManager>, use_session_bus: bool) -> Self {
        Self {
            manager,
            use_session_bus,
        }
    }

    /// Start monitoring logind events.
    ///
    /// This spawns background tasks for:
    /// - Shutdown inhibitor lock management
    /// - PrepareForShutdown signal handling
    /// - Session tracking for on_no_session
    ///
    /// Returns immediately after spawning tasks.
    pub async fn start(&self) -> anyhow::Result<()> {
        // Connect to system bus for logind (always system, even in dev mode)
        let connection = if self.use_session_bus {
            info!("Skipping logind monitoring in session bus mode (development)");
            return Ok(());
        } else {
            zbus::Connection::system().await?
        };

        let proxy = Login1ManagerProxy::new(&connection).await?;

        // Take shutdown inhibitor lock
        let inhibitor_fd = match proxy
            .inhibit("shutdown", "tisvlay", "Turning off TVs", "delay")
            .await
        {
            Ok(fd) => {
                info!("Acquired shutdown inhibitor lock");
                Some(fd)
            }
            Err(e) => {
                warn!(error = %e, "Failed to acquire shutdown inhibitor lock");
                None
            }
        };

        // Spawn PrepareForShutdown handler
        let manager = self.manager.clone();
        let proxy_for_shutdown = Login1ManagerProxy::new(&connection).await?;
        tokio::spawn(async move {
            handle_prepare_for_shutdown(proxy_for_shutdown, manager, inhibitor_fd).await;
        });

        // Spawn PrepareForSleep handler
        let manager = self.manager.clone();
        let proxy_for_sleep = Login1ManagerProxy::new(&connection).await?;
        tokio::spawn(async move {
            handle_prepare_for_sleep(proxy_for_sleep, manager).await;
        });

        // Spawn session tracking
        let manager = self.manager.clone();
        let proxy_for_sessions = Login1ManagerProxy::new(&connection).await?;
        let connection_for_sessions = connection.clone();
        tokio::spawn(async move {
            handle_session_changes(proxy_for_sessions, connection_for_sessions, manager).await;
        });

        Ok(())
    }
}

/// Handle PrepareForShutdown signal.
///
/// Queries `ScheduledShutdown` to distinguish poweroff/halt from reboot/kexec,
/// then executes the corresponding `on_shutdown` or `on_reboot` action for all
/// managed TVs before releasing the inhibitor lock.
async fn handle_prepare_for_shutdown(
    proxy: Login1ManagerProxy<'static>,
    manager: Arc<TvManager>,
    inhibitor_fd: Option<zbus::zvariant::OwnedFd>,
) {
    use futures_util::StreamExt;

    let mut stream = match proxy.receive_prepare_for_shutdown().await {
        Ok(stream) => stream,
        Err(e) => {
            error!(error = %e, "Failed to subscribe to PrepareForShutdown signal");
            return;
        }
    };

    while let Some(signal) = stream.next().await {
        let args = match signal.args() {
            Ok(args) => args,
            Err(e) => {
                warn!(error = %e, "Failed to parse PrepareForShutdown args");
                continue;
            }
        };

        if *args.start() {
            // Determine shutdown type from logind's ScheduledShutdown property
            let (shutdown_type, event) = match proxy.scheduled_shutdown().await {
                Ok((type_str, _usec)) => {
                    let event = match type_str.as_str() {
                        "reboot" | "kexec" => SystemEvent::Reboot,
                        _ => SystemEvent::Shutdown, // poweroff, halt, or unknown
                    };
                    (type_str, event)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to query ScheduledShutdown, assuming poweroff");
                    ("poweroff".to_string(), SystemEvent::Shutdown)
                }
            };

            info!(shutdown_type = %shutdown_type, "System is preparing for shutdown");

            manager.execute_system_event(event, &shutdown_type).await;

            // Release inhibitor lock by dropping the fd
            if let Some(fd) = inhibitor_fd {
                drop(fd);
                info!("Released shutdown inhibitor lock");
            }
            break;
        }
    }
}

/// Handle PrepareForSleep signal.
///
/// Unlike shutdown, sleep/wake can happen multiple times per session,
/// so this handler loops. No inhibitor lock is needed — sleep is fast
/// and TVs can handle the commands asynchronously.
async fn handle_prepare_for_sleep(proxy: Login1ManagerProxy<'static>, manager: Arc<TvManager>) {
    use futures_util::StreamExt;

    let mut stream = match proxy.receive_prepare_for_sleep().await {
        Ok(stream) => stream,
        Err(e) => {
            error!(error = %e, "Failed to subscribe to PrepareForSleep signal");
            return;
        }
    };

    while let Some(signal) = stream.next().await {
        let args = match signal.args() {
            Ok(args) => args,
            Err(e) => {
                warn!(error = %e, "Failed to parse PrepareForSleep args");
                continue;
            }
        };

        if *args.start() {
            info!("System is preparing for sleep");
            manager
                .execute_system_event(SystemEvent::Sleep, "sleep")
                .await;
        } else {
            info!("System resumed from sleep");
            // Notify the forwarder before executing on_wake, so it can
            // suppress NM reconnect attempts that would race with WoL.
            manager.emit_system_wake();
            manager
                .execute_system_event(SystemEvent::Wake, "wake")
                .await;
        }
    }
}

/// Handle session changes to detect when no user sessions are active.
async fn handle_session_changes(
    proxy: Login1ManagerProxy<'static>,
    connection: zbus::Connection,
    manager: Arc<TvManager>,
) {
    use futures_util::StreamExt;

    // Check initial session state before subscribing to signals.
    // If the daemon starts when no user sessions exist (e.g. after a graphical
    // session has ended or before the DM has started), on_no_session would
    // never fire without this initial check because no SessionRemoved signal
    // will arrive to trigger it.
    check_active_sessions(&proxy, &connection, &manager).await;

    // Subscribe to both SessionNew and SessionRemoved
    let mut new_stream = match proxy.receive_session_new().await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to subscribe to SessionNew signal");
            return;
        }
    };

    let mut removed_stream = match proxy.receive_session_removed().await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to subscribe to SessionRemoved signal");
            return;
        }
    };

    loop {
        tokio::select! {
            Some(signal) = new_stream.next() => {
                if let Ok(args) = signal.args() {
                    debug!(
                        session_id = %args.session_id(),
                        "Session created"
                    );
                    // Do NOT call check_active_sessions here: the new session
                    // object may not yet have Active=true when the D-Bus
                    // round-trips complete (the seat hasn't switched yet).
                    // Checking here causes a race where on_no_session can
                    // fire in the middle of a login transition.
                    // We only run the check when sessions disappear.
                }
            }
            Some(signal) = removed_stream.next() => {
                if let Ok(args) = signal.args() {
                    debug!(
                        session_id = %args.session_id(),
                        "Session removed"
                    );
                    // Check if all user sessions are gone
                    check_active_sessions(&proxy, &connection, &manager).await;
                }
            }
            else => break,
        }
    }
}

/// Check if there are any active graphical user sessions.
///
/// If no active graphical user sessions exist, execute `on_no_session`.
async fn check_active_sessions(
    proxy: &Login1ManagerProxy<'_>,
    connection: &zbus::Connection,
    manager: &TvManager,
) {
    let sessions = match proxy.list_sessions().await {
        Ok(sessions) => sessions,
        Err(e) => {
            warn!(error = %e, "Failed to list sessions");
            return;
        }
    };

    let mut has_active_graphical_user = false;

    for (_session_id, _uid, _user_name, _seat_id, session_path) in &sessions {
        // Check if this is an active graphical user session
        let session_proxy = match Login1SessionProxy::builder(connection).path(session_path.clone())
        {
            Ok(builder) => match builder.build().await {
                Ok(proxy) => proxy,
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        let class = session_proxy.class().await.unwrap_or_default();
        let active = session_proxy.active().await.unwrap_or(false);

        if class == "user" && active {
            has_active_graphical_user = true;
            break;
        }
    }

    if !has_active_graphical_user {
        debug!("No active graphical user sessions detected");
        manager
            .execute_system_event(SystemEvent::NoSession, "no_session")
            .await;
    }
}
