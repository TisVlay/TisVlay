//! Hand-rolled fake `org.freedesktop.ScreenSaver`.
//!
//! Enough of the real interface for the session service's
//! `SessionMonitor` to subscribe to `ActiveChanged` signals
//! on a private test bus.
//!
//! Usage:
//!
//! ```no_run
//! # async fn run() -> zbus::Result<()> {
//! use tisvlay_test::{fake_screensaver::FakeScreenSaver, PrivateBus};
//!
//! let bus = PrivateBus::shared();
//! let conn = bus.connect().await?;
//! let ss = FakeScreenSaver::register(&conn).await?;
//!
//! // Simulate screen lock
//! ss.emit_active_changed(true).await?;
//! // Simulate screen unlock
//! ss.emit_active_changed(false).await?;
//! # Ok(()) }
//! ```

use tracing::{debug, instrument};
use zbus::object_server::{InterfaceRef, SignalEmitter};

const SERVICE_NAME: &str = "org.freedesktop.ScreenSaver";
const OBJECT_PATH: &str = "/org/freedesktop/ScreenSaver";

/// Minimal `org.freedesktop.ScreenSaver` stub registered on a test bus.
pub struct FakeScreenSaver {
    iface: InterfaceRef<ScreenSaverIface>,
}

impl std::fmt::Debug for FakeScreenSaver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeScreenSaver").finish_non_exhaustive()
    }
}

impl FakeScreenSaver {
    /// Register the fake on the given connection, acquiring
    /// `org.freedesktop.ScreenSaver` as a well-known name.
    #[instrument(skip_all)]
    pub async fn register(conn: &zbus::Connection) -> zbus::Result<Self> {
        conn.object_server()
            .at(OBJECT_PATH, ScreenSaverIface { idle_time: 0 })
            .await?;
        conn.request_name(SERVICE_NAME).await?;
        let iface = conn
            .object_server()
            .interface::<_, ScreenSaverIface>(OBJECT_PATH)
            .await?;
        Ok(Self { iface })
    }

    /// Emit `ActiveChanged(active)` on the bus.
    ///
    /// `active = true` means locked, `active = false` means unlocked.
    #[instrument(skip(self))]
    pub async fn emit_active_changed(&self, active: bool) -> zbus::Result<()> {
        debug!(active, "fake screensaver emitting ActiveChanged");
        ScreenSaverIface::active_changed(self.iface.signal_emitter(), active).await
    }

    /// Set the idle time returned by `GetSessionIdleTime`.
    pub async fn set_idle_time(&self, seconds: u32) {
        let mut iface = self.iface.get_mut().await;
        iface.idle_time = seconds;
    }
}

pub(crate) struct ScreenSaverIface {
    idle_time: u32,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaverIface {
    fn get_session_idle_time(&self) -> u32 {
        self.idle_time
    }

    #[zbus(signal)]
    async fn active_changed(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;
}
