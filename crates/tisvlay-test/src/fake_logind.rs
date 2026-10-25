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
//! let bus = PrivateBus::isolated(); // owns a well-known name: a bus of its own
//! let conn = bus.connect().await?;
//! let logind = FakeLogind::register(&conn).await?;
//!
//! // ... daemon / session code subscribes on the same bus ...
//!
//! logind.emit_prepare_for_sleep(true).await?;
//! logind.set_locked(true).await?; // drives the lock watcher
//! # Ok(()) }
//! ```

#![expect(
    unsafe_code,
    reason = "the fake reads the test process's own uid (libc::getuid)"
)]

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
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
    releases: Arc<Mutex<Vec<Instant>>>,
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
        let releases = Arc::new(Mutex::new(Vec::new()));
        let manager = ManagerIface {
            releases: Arc::clone(&releases),
            inhibit_delay_max_usec: DEFAULT_INHIBIT_DELAY_MAX_USEC,
        };
        conn.object_server().at(OBJECT_PATH, manager).await?;
        conn.object_server()
            .at(
                SESSION_PATH,
                SessionIface {
                    locked: false,
                    lock_requests: 0,
                },
            )
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
        Ok(Self {
            manager,
            session,
            releases,
        })
    }

    /// How many times the graphical session was asked to lock.
    pub async fn lock_requests(&self) -> u32 {
        self.session.get().await.lock_requests
    }

    /// When each inhibitor the fake handed out was released (its fd closed
    /// by every holder), in release order.
    pub fn inhibitor_releases(&self) -> Vec<Instant> {
        self.releases.lock().clone()
    }

    /// Set `InhibitDelayMaxUSec`, how long a delay inhibitor may hold a
    /// sleep (default 5 s, logind's own).
    pub async fn set_inhibit_delay_max(&self, delay: Duration) {
        self.manager.get_mut().await.inhibit_delay_max_usec =
            u64::try_from(delay.as_micros()).expect("a test's inhibit delay fits in u64 µs");
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

/// logind's default `InhibitDelayMaxSec=`, 5 s, in microseconds.
const DEFAULT_INHIBIT_DELAY_MAX_USEC: u64 = 5_000_000;

pub(crate) struct ManagerIface {
    releases: Arc<Mutex<Vec<Instant>>>,
    inhibit_delay_max_usec: u64,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl ManagerIface {
    /// Hand out the write end of a pipe, as the real logind does, and
    /// record when it's released: the read end sees EOF once every
    /// holder has closed it.
    async fn inhibit(
        &self,
        _what: &str,
        _who: &str,
        _why: &str,
        _mode: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        let (mut reader, writer) = std::io::pipe()
            .map_err(|e| zbus::fdo::Error::Failed(format!("inhibitor pipe: {e}")))?;
        let releases = Arc::clone(&self.releases);
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            while matches!(reader.read(&mut byte), Ok(n) if n > 0) {}
            releases.lock().push(Instant::now());
        });
        Ok(zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(
            writer,
        )))
    }

    #[zbus(property, name = "InhibitDelayMaxUSec")]
    fn inhibit_delay_max_usec(&self) -> u64 {
        self.inhibit_delay_max_usec
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
    lock_requests: u32,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl SessionIface {
    /// Count the request; the real logind tells the desktop's locker,
    /// which sets `LockedHint` once the screen is locked.
    async fn lock(&mut self) {
        self.lock_requests += 1;
    }

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
