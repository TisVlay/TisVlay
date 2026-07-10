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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, instrument};
use zbus::object_server::{InterfaceRef, SignalEmitter};

const SERVICE_NAME: &str = "org.freedesktop.ScreenSaver";
const OBJECT_PATH: &str = "/org/freedesktop/ScreenSaver";

/// Minimal `org.freedesktop.ScreenSaver` stub registered on a test bus.
pub struct FakeScreenSaver {
    iface: InterfaceRef<ScreenSaverIface>,
    active: Arc<AtomicBool>,
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
        let active = Arc::new(AtomicBool::new(false));
        conn.object_server()
            .at(
                OBJECT_PATH,
                ScreenSaverIface {
                    active: active.clone(),
                },
            )
            .await?;
        conn.request_name(SERVICE_NAME).await?;
        let iface = conn
            .object_server()
            .interface::<_, ScreenSaverIface>(OBJECT_PATH)
            .await?;
        Ok(Self { iface, active })
    }

    /// Emit `ActiveChanged(active)` on the bus and record the state for
    /// `GetActive` — a lock that predates a subscriber is still
    /// queryable, matching the real interface.
    ///
    /// `active = true` means locked, `active = false` means unlocked.
    #[instrument(skip(self))]
    pub async fn emit_active_changed(&self, active: bool) -> zbus::Result<()> {
        debug!(active, "fake screensaver emitting ActiveChanged");
        self.active.store(active, Ordering::SeqCst);
        ScreenSaverIface::active_changed(self.iface.signal_emitter(), active).await
    }
}

pub(crate) struct ScreenSaverIface {
    active: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaverIface {
    #[zbus(signal)]
    async fn active_changed(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;

    /// Current lock state, for subscribers that arrived after the edge.
    async fn get_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}
