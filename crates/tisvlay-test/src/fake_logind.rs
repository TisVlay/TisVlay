//! Hand-rolled fake `org.freedesktop.login1`.
//!
//! Just enough of the real logind interface for the daemon's
//! shutdown-inhibitor / prepare-for-sleep handling and the session
//! service's lock watcher to run against a private test bus. Emits
//! `PrepareForSleep` signals on demand, and exposes one graphical session
//! whose `LockedHint` can be toggled (emitting `PropertiesChanged`).
//!
//! Usage:
//!
//! ```no_run
//! # async fn run() -> zbus::Result<()> {
//! use tisvlay_test::{fake_logind::FakeLogind, PrivateBus};
//!
//! let bus = PrivateBus::shared();
//! let conn = bus.connect().await?;
//! let logind = FakeLogind::register(&conn).await?;
//!
//! // ... daemon / session code subscribes on the same bus ...
//!
//! logind.emit_prepare_for_sleep(true).await?;
//! logind.set_locked(true).await?; // drives the lock watcher
//! # Ok(()) }
//! ```

use tracing::{debug, instrument};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

const SERVICE_NAME: &str = "org.freedesktop.login1";
const OBJECT_PATH: &str = "/org/freedesktop/login1";
/// The one graphical session the fake exposes; `ListSessions` returns it
/// and the lock watcher reads `LockedHint` / `Type` off it.
const SESSION_ID: &str = "test";
const SESSION_PATH: &str = "/org/freedesktop/login1/session/test";

/// Minimal `login1` stub (Manager + one graphical Session) registered on a
/// shared test bus.
///
/// Holds `InterfaceRef`s so tests can synthesize signals and property
/// changes without re-looking-up the interfaces on every call.
pub struct FakeLogind {
    manager: InterfaceRef<ManagerIface>,
    session: InterfaceRef<SessionIface>,
}

impl std::fmt::Debug for FakeLogind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeLogind").finish_non_exhaustive()
    }
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
        conn.object_server()
            .at(SESSION_PATH, SessionIface { locked: false })
            .await?;
        conn.request_name(SERVICE_NAME).await?;
        let manager = conn
            .object_server()
            .interface::<_, ManagerIface>(OBJECT_PATH)
            .await?;
        let session = conn
            .object_server()
            .interface::<_, SessionIface>(SESSION_PATH)
            .await?;
        Ok(Self { manager, session })
    }

    /// Emit `PrepareForSleep(start)` on the bus.
    #[instrument(skip(self))]
    pub async fn emit_prepare_for_sleep(&self, start: bool) -> zbus::Result<()> {
        debug!(start, "fake logind emitting PrepareForSleep");
        ManagerIface::prepare_for_sleep(self.manager.signal_emitter(), start).await
    }

    /// Set the graphical session's `LockedHint`, emitting the
    /// `PropertiesChanged` the lock watcher follows. Toggling before the
    /// watcher subscribes leaves the value for its `GetActive`-style seed.
    #[instrument(skip(self))]
    pub async fn set_locked(&self, locked: bool) -> zbus::Result<()> {
        debug!(locked, "fake logind setting LockedHint");
        let mut iface = self.session.get_mut().await;
        iface.locked = locked;
        iface
            .locked_hint_changed(self.session.signal_emitter())
            .await
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

    /// The lock watcher's hydration source: one graphical session owned by
    /// this (the test) process's uid, so the own-uid filter admits it.
    async fn list_sessions(&self) -> Vec<(String, u32, String, String, OwnedObjectPath)> {
        // SAFETY: `getuid` is always successful and has no preconditions.
        let uid = unsafe { libc::getuid() };
        let path = ObjectPath::try_from(SESSION_PATH)
            .expect("static session path is valid")
            .into();
        vec![(SESSION_ID.into(), uid, "test".into(), "seat0".into(), path)]
    }

    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}

/// One graphical `login1.Session` with a toggleable `LockedHint`.
pub(crate) struct SessionIface {
    locked: bool,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl SessionIface {
    /// `true` while the fake reports the screen locked.
    #[zbus(property)]
    async fn locked_hint(&self) -> bool {
        self.locked
    }

    /// Owned by this (the test) process's uid, so the own-uid filter on
    /// the `SessionNew` path admits it.
    #[zbus(property)]
    async fn user(&self) -> (u32, OwnedObjectPath) {
        // SAFETY: `getuid` is always successful and has no preconditions.
        let uid = unsafe { libc::getuid() };
        let path = ObjectPath::try_from(format!("/org/freedesktop/login1/user/_{uid}"))
            .expect("uid path is valid")
            .into();
        (uid, path)
    }

    /// On a seat, so the watcher's local-session filter admits it (a
    /// seatless SSH session would be skipped).
    #[zbus(property)]
    async fn seat(&self) -> (String, OwnedObjectPath) {
        let path = ObjectPath::try_from("/org/freedesktop/login1/seat/seat0")
            .expect("static seat path is valid")
            .into();
        ("seat0".into(), path)
    }

    /// The foreground session, so it wins the active-session preference.
    #[zbus(property)]
    async fn active(&self) -> bool {
        true
    }

    /// A graphical session `Type`, for realism (the lock watcher keys on
    /// the seat, not the type).
    #[zbus(property, name = "Type")]
    async fn session_type(&self) -> String {
        "wayland".into()
    }
}
