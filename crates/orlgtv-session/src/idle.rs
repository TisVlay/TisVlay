//! Idle timeout detection.
//!
//! Polls the D-Bus ScreenSaver interface to detect when the user has been idle
//! for a configured duration.

use crate::session_monitor::{get_session_idle_time, SessionEvent};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Monitors user idle time and emits events.
pub struct IdleMonitor {
    timeout_seconds: u64,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
}

impl IdleMonitor {
    /// Create a new idle monitor.
    ///
    /// # Arguments
    /// - `timeout_seconds`: How long the user must be idle before triggering (0 to disable)
    /// - `event_tx`: Channel to send idle events
    pub fn new(timeout_seconds: u64, event_tx: mpsc::UnboundedSender<SessionEvent>) -> Self {
        Self {
            timeout_seconds,
            event_tx,
        }
    }

    /// Check if idle monitoring is enabled.
    pub fn is_enabled(&self) -> bool {
        self.timeout_seconds > 0
    }

    /// Start the idle monitoring loop.
    ///
    /// This polls the session idle time and emits `IdleTimeout` when the user
    /// has been idle for longer than the configured timeout, and `IdleResume`
    /// when activity is detected after being idle.
    pub async fn run(&self) {
        if !self.is_enabled() {
            debug!("Idle monitoring disabled (timeout = 0)");
            return;
        }

        info!(
            timeout_seconds = self.timeout_seconds,
            "Starting idle monitor"
        );

        // Connect to session bus
        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to connect to session D-Bus for idle monitoring: {}", e);
                return;
            }
        };

        // Poll interval: check every 1/4 of the timeout, minimum 5 seconds
        let poll_interval = Duration::from_secs((self.timeout_seconds / 4).max(5));

        let mut was_idle = false;

        loop {
            tokio::time::sleep(poll_interval).await;

            match get_session_idle_time(&conn).await {
                Ok(idle_seconds) => {
                    let is_idle = idle_seconds as u64 >= self.timeout_seconds;

                    if is_idle && !was_idle {
                        // Transitioned to idle
                        debug!(
                            idle_seconds = idle_seconds,
                            timeout = self.timeout_seconds,
                            "User idle timeout reached"
                        );
                        if self.event_tx.send(SessionEvent::IdleTimeout).is_err() {
                            warn!("Event channel closed, stopping idle monitor");
                            break;
                        }
                        was_idle = true;
                    } else if !is_idle && was_idle {
                        // Transitioned from idle to active
                        debug!(idle_seconds = idle_seconds, "User activity resumed");
                        if self.event_tx.send(SessionEvent::IdleResume).is_err() {
                            warn!("Event channel closed, stopping idle monitor");
                            break;
                        }
                        was_idle = false;
                    }
                }
                Err(e) => {
                    // This can happen if the ScreenSaver service isn't available
                    // Log once and continue polling
                    debug!("Failed to get session idle time: {}", e);
                }
            }
        }

        info!("Idle monitor stopped");
    }
}
