//! Hand-rolled fake `org.freedesktop.login1.Manager`.
//!
//! Just enough of the real logind interface for the daemon's
//! shutdown-inhibitor / prepare-for-sleep handling to run against a
//! private test bus. Emits `PrepareForSleep` signals on demand.
//!
//! Usage:
//!
//! ```no_run
//! # async fn run() -> zbus::Result<()> {
//! use tisvlay_test_support::{fake_logind::FakeLogind, PrivateBus};
//!
//! let bus = PrivateBus::shared();
//! let conn = bus.connect().await?;
//! let logind = FakeLogind::register(&conn).await?;
//!
//! // ... daemon code subscribes to the signal on the same bus ...
//!
//! logind.emit_prepare_for_sleep(true).await?;
//! # Ok(()) }
//! ```

use tracing::instrument;
use zbus::object_server::{InterfaceRef, SignalEmitter};

const SERVICE_NAME: &str = "org.freedesktop.login1";
const OBJECT_PATH: &str = "/org/freedesktop/login1";

/// Minimal `login1.Manager` stub registered on a shared test bus.
///
/// Holds an `InterfaceRef` so tests can synthesize signals without
/// re-looking-up the interface on every call.
pub struct FakeLogind {
    iface: InterfaceRef<ManagerIface>,
}

impl FakeLogind {
    /// Register the fake on the given connection, acquiring
    /// `org.freedesktop.login1` as a well-known name.
    ///
    /// Only one `FakeLogind` may exist per bus — registering twice
    /// fails because name ownership is bus-global.
    #[instrument(skip_all)]
    pub async fn register(conn: &zbus::Connection) -> zbus::Result<Self> {
        conn.object_server().at(OBJECT_PATH, ManagerIface).await?;
        conn.request_name(SERVICE_NAME).await?;
        let iface = conn
            .object_server()
            .interface::<_, ManagerIface>(OBJECT_PATH)
            .await?;
        Ok(Self { iface })
    }

    /// Emit `PrepareForSleep(start)` on the bus.
    #[instrument(skip(self))]
    pub async fn emit_prepare_for_sleep(&self, start: bool) -> zbus::Result<()> {
        ManagerIface::prepare_for_sleep(self.iface.signal_emitter(), start).await
    }
}

pub(crate) struct ManagerIface;

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl ManagerIface {
    /// Accept inhibitor locks but do nothing with them — the daemon
    /// just needs the method call to succeed.
    async fn inhibit(
        &self,
        _what: &str,
        _who: &str,
        _why: &str,
        _mode: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        // Return a dup of /dev/null so the caller sees a valid fd it
        // can hold onto. The real logind returns an anonymous pipe
        // whose closure signals release; for tests we don't exercise
        // that lifecycle and the fd is just a placeholder.
        let file = std::fs::File::open("/dev/null")
            .map_err(|e| zbus::fdo::Error::Failed(format!("open /dev/null: {e}")))?;
        use std::os::fd::{IntoRawFd, OwnedFd};
        // SAFETY: we own the fd from `File::open`, and `into_raw_fd`
        // transfers ownership out without closing it, so wrapping in
        // `OwnedFd::from_raw_fd` re-takes that ownership exactly once.
        let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(file.into_raw_fd()) };
        Ok(zbus::zvariant::OwnedFd::from(owned))
    }

    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}
