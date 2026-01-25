//! Session D-Bus event monitoring.
//!
//! Monitors freedesktop D-Bus signals for screen lock/unlock and suspend/resume events.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use zbus::proxy;

/// Events detected from the desktop session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// Screen was locked.
    ScreenLocked,
    /// Screen was unlocked.
    ScreenUnlocked,
    /// User interaction detected while locked (greeter becoming visible).
    /// This fires when the user moves the mouse or presses a key while locked,
    /// before they actually unlock. Detected via Wayland ext-idle-notify-v1 protocol.
    GreeterWake,
    /// System is about to suspend/hibernate.
    PrepareForSleep,
    /// System just resumed from suspend/hibernate.
    ResumeFromSleep,
    /// User has been idle for the configured timeout.
    IdleTimeout,
    /// User activity detected after idle.
    IdleResume,
}

impl std::fmt::Display for SessionEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScreenLocked => write!(f, "ScreenLocked"),
            Self::ScreenUnlocked => write!(f, "ScreenUnlocked"),
            Self::GreeterWake => write!(f, "GreeterWake"),
            Self::PrepareForSleep => write!(f, "PrepareForSleep"),
            Self::ResumeFromSleep => write!(f, "ResumeFromSleep"),
            Self::IdleTimeout => write!(f, "IdleTimeout"),
            Self::IdleResume => write!(f, "IdleResume"),
        }
    }
}

/// D-Bus proxy for org.freedesktop.ScreenSaver.
#[proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    /// Signal emitted when screen saver/lock state changes.
    /// `active` is true when screen is locked, false when unlocked.
    #[zbus(signal)]
    fn active_changed(&self, active: bool) -> zbus::Result<()>;

    /// Get the session idle time in seconds.
    /// Used by the idle timeout feature.
    fn get_session_idle_time(&self) -> zbus::Result<u32>;
}

/// D-Bus proxy for org.freedesktop.login1.Manager.
#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    /// Signal emitted before sleep/hibernate.
    /// `start` is true before sleep, false after wake.
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Monitors D-Bus session events.
pub struct SessionMonitor {
    event_tx: mpsc::UnboundedSender<SessionEvent>,
}

impl SessionMonitor {
    /// Create a new session monitor.
    pub fn new(event_tx: mpsc::UnboundedSender<SessionEvent>) -> Self {
        Self { event_tx }
    }

    /// Start monitoring session events.
    ///
    /// This spawns background tasks to listen for D-Bus signals.
    /// Returns an `Arc<AtomicBool>` that tracks whether the screen is currently locked.
    /// This can be shared with the Wayland idle monitor for greeter wake detection.
    pub async fn start(&self) -> Result<Arc<AtomicBool>> {
        // Connect to session bus for ScreenSaver signals
        let session_conn = zbus::Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;

        // Connect to system bus for login1 signals
        let system_conn = zbus::Connection::system()
            .await
            .context("Failed to connect to system D-Bus")?;

        // Shared state for tracking if screen is locked
        let is_locked = Arc::new(AtomicBool::new(false));

        // Start ScreenSaver monitor
        let tx1 = self.event_tx.clone();
        let is_locked_clone = is_locked.clone();
        tokio::spawn(async move {
            if let Err(e) = monitor_screen_saver(session_conn, tx1, is_locked_clone).await {
                error!("ScreenSaver monitor failed: {}", e);
            }
        });

        // Start login1 monitor
        let tx2 = self.event_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = monitor_login1(system_conn, tx2).await {
                error!("login1 monitor failed: {}", e);
            }
        });

        info!("Session monitor started");
        Ok(is_locked)
    }
}

/// Monitor org.freedesktop.ScreenSaver signals.
async fn monitor_screen_saver(
    conn: zbus::Connection,
    tx: mpsc::UnboundedSender<SessionEvent>,
    is_locked: Arc<AtomicBool>,
) -> Result<()> {
    let proxy = ScreenSaverProxy::new(&conn)
        .await
        .context("Failed to create ScreenSaver proxy")?;

    let mut stream = proxy
        .receive_active_changed()
        .await
        .context("Failed to subscribe to ActiveChanged signal")?;

    info!("Subscribed to org.freedesktop.ScreenSaver.ActiveChanged");

    while let Some(signal) = stream.next().await {
        match signal.args() {
            Ok(args) => {
                let active = args.active;

                // Update locked state for Wayland greeter wake detection
                is_locked.store(active, Ordering::SeqCst);

                let event = if active {
                    SessionEvent::ScreenLocked
                } else {
                    SessionEvent::ScreenUnlocked
                };
                debug!(event = %event, "ScreenSaver signal received");
                if tx.send(event).is_err() {
                    warn!("Event channel closed");
                    break;
                }
            }
            Err(e) => {
                warn!("Failed to parse ActiveChanged signal: {}", e);
            }
        }
    }

    Ok(())
}

/// Monitor org.freedesktop.login1.Manager signals.
async fn monitor_login1(
    conn: zbus::Connection,
    tx: mpsc::UnboundedSender<SessionEvent>,
) -> Result<()> {
    let proxy = Login1ManagerProxy::new(&conn)
        .await
        .context("Failed to create login1 Manager proxy")?;

    let mut stream = proxy
        .receive_prepare_for_sleep()
        .await
        .context("Failed to subscribe to PrepareForSleep signal")?;

    info!("Subscribed to org.freedesktop.login1.Manager.PrepareForSleep");

    while let Some(signal) = stream.next().await {
        match signal.args() {
            Ok(args) => {
                let start = args.start;
                let event = if start {
                    SessionEvent::PrepareForSleep
                } else {
                    SessionEvent::ResumeFromSleep
                };
                debug!(event = %event, "login1 signal received");
                if tx.send(event).is_err() {
                    warn!("Event channel closed");
                    break;
                }
            }
            Err(e) => {
                warn!("Failed to parse PrepareForSleep signal: {}", e);
            }
        }
    }

    Ok(())
}

/// Get the session idle time in seconds from the ScreenSaver interface.
///
/// Used by the idle timeout feature. Note: This may not be supported on all
/// platforms (e.g., KDE Plasma Wayland). The idle.rs module handles failures gracefully.
pub async fn get_session_idle_time(conn: &zbus::Connection) -> Result<u32> {
    let proxy = ScreenSaverProxy::new(conn)
        .await
        .context("Failed to create ScreenSaver proxy")?;

    proxy
        .get_session_idle_time()
        .await
        .context("Failed to get session idle time")
}
