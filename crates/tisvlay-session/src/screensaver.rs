//! Consumed-D-Bus lock/unlock adapter: `org.freedesktop.ScreenSaver`
//! `ActiveChanged` on the session bus → session events. The interface is
//! the same one `tisvlay-test`'s `FakeScreenSaver` stubs, so the adapter
//! is testable on a private bus.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use futures_util::StreamExt;
use tracing::info;

use crate::{SessionClient, SessionEvent};

#[zbus::proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    /// `active = true` means locked, `false` means unlocked.
    #[zbus(signal)]
    fn active_changed(&self, active: bool) -> zbus::Result<()>;
}

/// Forward lock/unlock transitions until the bus or the core loop drops.
/// `is_locked` mirrors the lock state for the wayland idle adapter's
/// lock-wake policy.
pub async fn watch_screensaver(
    connection: zbus::Connection,
    client: SessionClient,
    is_locked: Arc<AtomicBool>,
) -> Result<()> {
    let proxy = ScreenSaverProxy::new(&connection).await?;
    let mut stream = proxy.receive_active_changed().await?;
    info!("screensaver watcher running");
    while let Some(signal) = stream.next().await {
        let active = signal.args()?.active;
        is_locked.store(active, Ordering::SeqCst);
        let event = if active {
            SessionEvent::Lock
        } else {
            SessionEvent::Unlock
        };
        if !client.send(event) {
            return Ok(());
        }
    }
    Ok(())
}
