//! logind lock/unlock adapter: `org.freedesktop.login1.Session`
//! `LockedHint` → session events.
//!
//! `LockedHint` is the one standardized, cross-desktop lock-state signal:
//! the session's screen locker writes it (`SetLockedHint`), and both KDE
//! (kscreenlocker) and GNOME (gnome-shell) do. It replaces the de-facto
//! `org.freedesktop.ScreenSaver` `ActiveChanged` interface, which is a
//! KDE-only extension — GNOME's `gsd-screensaver` implements only the
//! standardized idle-inhibit subset of that name and never signals lock
//! state. The logind `Lock`/`Unlock` *signals* are deliberately unused:
//! they are external lock *requests* and do not fire on a DE-initiated
//! lock; `LockedHint` (the property) is the state the locker sets.
//!
//! The service runs in the user@ manager scope, not a logind session
//! (`GetSessionByPID` fails for it), so the graphical seat session is
//! found by enumeration — this user's `x11`/`wayland` session — the same
//! own-uid + graphical filter the remote logind detector uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use tracing::debug;

use crate::detector::Backoff;
use crate::logind::{is_graphical, own_uid};
use crate::{SessionClient, SessionEvent};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    /// `(id, uid, user_name, seat, object_path)` for each live session.
    fn list_sessions(
        &self,
    ) -> zbus::Result<Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)>>;

    /// A session as it appears — id + path; properties are read off it.
    #[zbus(signal)]
    fn session_new(&self, id: String, path: zbus::zvariant::OwnedObjectPath) -> zbus::Result<()>;

    /// A session as it goes away.
    #[zbus(signal)]
    fn session_removed(
        &self,
        id: String,
        path: zbus::zvariant::OwnedObjectPath,
    ) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    /// `true` while the session's locker reports the screen locked.
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;

    /// `x11`/`wayland` for graphical logins, `tty`/`unspecified` otherwise.
    #[zbus(property, name = "Type")]
    fn session_type(&self) -> zbus::Result<String>;
}

/// Spawn the lock watcher. Reports `Lock`/`Unlock` into the session core;
/// the task supervises its own system-bus connection and never blocks the
/// caller. `is_locked` mirrors the state for the idle adapter's lock-wake
/// policy and the core's drift re-assert.
pub fn spawn(client: SessionClient, is_locked: Arc<AtomicBool>) {
    tokio::spawn(run(client, is_locked));
}

/// Supervised watch loop: connect, hydrate + follow `LockedHint`, and run
/// until the connection or the watched session ends; back off and retry.
/// Quiet after the first warning so a box without logind does not spam.
async fn run(client: SessionClient, is_locked: Arc<AtomicBool>) {
    let mut backoff = Backoff::default();
    loop {
        let outcome = match zbus::Connection::system().await {
            Ok(connection) => watch(&connection, &client, &is_locked).await,
            Err(e) => Err(Fault::Bus(e)),
        };
        match outcome {
            Ok(()) => {
                debug!("lock watcher: session ended; reconnecting");
                backoff.reset();
            }
            Err(Fault::CoreGone) => return,
            Err(Fault::Bus(e)) => backoff.note_failure("lock watcher: system bus", e),
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// One watch session over an injected `connection` — the entry integration
/// tests drive against a private bus. Production goes through the
/// supervised [`spawn`]; a dead core or a spent session both resolve to
/// `Ok(())` (the caller/test simply stops), only a bus fault surfaces.
pub async fn watch_lock(
    connection: zbus::Connection,
    client: SessionClient,
    is_locked: Arc<AtomicBool>,
) -> zbus::Result<()> {
    match watch(&connection, &client, &is_locked).await {
        Ok(()) | Err(Fault::CoreGone) => Ok(()),
        Err(Fault::Bus(e)) => Err(e),
    }
}

/// Why a watch session ended.
enum Fault {
    Bus(zbus::Error),
    /// The session core dropped the event channel — stop supervising.
    CoreGone,
}

impl From<zbus::Error> for Fault {
    fn from(e: zbus::Error) -> Self {
        Fault::Bus(e)
    }
}

/// One watch session: acquire this user's graphical session, seed the lock
/// state (no synthesized event — profile lock actions fire on edges
/// alone), then forward `LockedHint` edges until the session goes away.
async fn watch(
    connection: &zbus::Connection,
    client: &SessionClient,
    is_locked: &AtomicBool,
) -> Result<(), Fault> {
    let own_uid = own_uid();
    let manager = Login1ManagerProxy::new(connection).await?;
    let mut gone_stream = manager.receive_session_removed().await?;

    // Acquire the graphical seat session: hydrate from the live list, else
    // wait for one to appear (the service can start before the session).
    let (session_id, session) = acquire_graphical(connection, &manager, own_uid).await?;

    // Seed: the lock can predate this watch (auto-login lock, or a service
    // restart mid-lock). State only — a synthesized event would double-fire
    // a profile lock action already taken.
    if let Ok(locked) = session.locked_hint().await {
        is_locked.store(locked, Ordering::SeqCst);
    }
    let mut locked_changes = session.receive_locked_hint_changed().await;

    loop {
        tokio::select! {
            change = locked_changes.next() => {
                // Property stream ended (session object gone): reconnect and
                // re-hydrate a fresh session.
                let Some(change) = change else { return Ok(()) };
                let Ok(locked) = change.get().await else { continue };
                // Emit only on a real edge. zbus yields the current value
                // when the stream opens; that first item matches the seed
                // and must not synthesize a Lock/Unlock (which would blank
                // or wake a TV on every service restart).
                if locked == is_locked.swap(locked, Ordering::SeqCst) {
                    continue;
                }
                let event = if locked { SessionEvent::Lock } else { SessionEvent::Unlock };
                if !client.send(event) {
                    return Err(Fault::CoreGone);
                }
            }
            signal = gone_stream.next() => {
                let Some(signal) = signal else { return Ok(()) };
                let Ok(args) = signal.args() else { continue };
                // Our session logged out: reconnect to re-acquire whatever
                // graphical session replaces it.
                if *args.id() == session_id {
                    return Ok(());
                }
            }
        }
    }
}

/// Return this user's graphical session as `(id, proxy)`, hydrating from
/// `ListSessions` first, then waiting on `SessionNew` until one qualifies.
/// The `SessionNew` subscription is taken before the list so a session
/// appearing during hydration is not missed.
async fn acquire_graphical(
    connection: &zbus::Connection,
    manager: &Login1ManagerProxy<'_>,
    own_uid: u32,
) -> Result<(String, Login1SessionProxy<'static>), Fault> {
    let mut new_stream = manager.receive_session_new().await?;
    for (id, uid, _user, _seat, path) in manager.list_sessions().await.unwrap_or_default() {
        if uid == own_uid
            && let Some(session) = graphical_session(connection, &path).await
        {
            return Ok((id, session));
        }
    }
    // None yet — the graphical session may start after this service.
    loop {
        let Some(signal) = new_stream.next().await else {
            // Manager signal stream ended — treat as a bus drop to reconnect.
            return Err(Fault::Bus(zbus::Error::InvalidReply));
        };
        let Ok(args) = signal.args() else { continue };
        if let Some(session) = graphical_session(connection, args.path()).await {
            return Ok((args.id().to_string(), session));
        }
    }
}

/// A `Session` proxy for `path` if it is a graphical session, else `None`.
/// The caller has already matched the uid (`ListSessions`) or accepts any
/// uid (`SessionNew` on this user's bus is this user's session).
async fn graphical_session(
    connection: &zbus::Connection,
    path: &zbus::zvariant::OwnedObjectPath,
) -> Option<Login1SessionProxy<'static>> {
    let session = Login1SessionProxy::builder(connection)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    is_graphical(&session.session_type().await.ok()?).then_some(session)
}
