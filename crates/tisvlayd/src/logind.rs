//! logind integration for system lifecycle events.
//!
//! Monitors `org.freedesktop.login1` on the system bus for:
//! - `PrepareForShutdown`: execute `on_shutdown` or `on_reboot` action
//!   (distinguished via the `ScheduledShutdown` property)
//! - `PrepareForSleep`: execute `on_sleep` or `on_wake` action
//!
//! Takes a shutdown inhibitor lock so the daemon can gracefully
//! shut down TVs before the system powers off.

use crate::tv_manager::{SystemEvent, TvManager};
use std::sync::Arc;
use tracing::{error, info, warn};
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
}

// =============================================================================
// LogindMonitor
// =============================================================================

/// Monitors logind for system lifecycle events (shutdown, sleep).
#[derive(Debug)]
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
    /// - PrepareForSleep signal handling
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
            manager
                .execute_system_event(SystemEvent::Wake, "wake")
                .await;
        }
    }
}
