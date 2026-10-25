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
//! (`GetSessionByPID` fails for it), so this user's session is found by
//! enumeration: an own-uid session **on a seat**. Keying on the seat, not
//! the session `Type`, is deliberate — it admits a display-manager login
//! (`Type=wayland`/`x11`) and a `startx` login (`Type=tty`) alike, while
//! excluding seatless SSH sessions; the foreground (`Active`) one wins.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::detector::{Backoff, Fault};
use crate::logind::{Login1ManagerProxy, own_uid};
use crate::{SessionClient, SessionEvent};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    /// `true` while the session's locker reports the screen locked.
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;

    /// `(uid, path)` of the session owner — the own-uid discriminator on
    /// the `SessionNew` path, which carries no uid.
    #[zbus(property)]
    fn user(&self) -> zbus::Result<(u32, zbus::zvariant::OwnedObjectPath)>;

    /// `(seat_id, path)`; `seat_id` is empty for a seatless (SSH) session.
    #[zbus(property)]
    fn seat(&self) -> zbus::Result<(String, zbus::zvariant::OwnedObjectPath)>;

    /// Whether this is the seat's foreground session.
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;

    /// Ask the session to lock. logind lets the session's own user, with
    /// no polkit rule; the desktop's locker does the locking.
    fn lock(&self) -> zbus::Result<()>;
}

/// Spawn the locker: each request (naming the TV whose going-off event asked)
/// locks this user's seat session through logind on the system bus. A
/// request that finds no bus or no seat session warns and is dropped.
pub fn spawn_locker(requests: mpsc::UnboundedReceiver<Uuid>) {
    tokio::spawn(async move {
        let mut requests = requests;
        while let Some(tv) = requests.recv().await {
            match zbus::Connection::system().await {
                Ok(connection) => lock_for(&connection, tv).await,
                Err(e) => warn!(%tv, %e, "no system bus to lock the session"),
            }
        }
    });
}

/// The locker over an injected `connection`, the entry integration tests
/// drive against a private bus. Runs until the requests end.
pub async fn run_locker(connection: zbus::Connection, mut requests: mpsc::UnboundedReceiver<Uuid>) {
    while let Some(tv) = requests.recv().await {
        lock_for(&connection, tv).await;
    }
}

async fn lock_for(connection: &zbus::Connection, tv: Uuid) {
    match lock_own_seat_session(connection).await {
        Ok(true) => info!(%tv, "session locked"),
        Ok(false) => warn!(%tv, "no seat session to lock"),
        Err(e) => warn!(%tv, %e, "could not lock the session"),
    }
}

/// Lock this user's seat session; `false` when it has none.
async fn lock_own_seat_session(connection: &zbus::Connection) -> zbus::Result<bool> {
    let manager = Login1ManagerProxy::new(connection).await?;
    let sessions = manager.list_sessions().await?;
    let Some((_id, session)) = scan_sessions_for_own_seat(connection, sessions, own_uid()).await
    else {
        return Ok(false);
    };
    session.lock().await?;
    Ok(true)
}

/// Spawn the lock watcher. Reports `Lock`/`Unlock` into the session core;
/// the task supervises its own system-bus connection and never blocks the
/// caller. `is_locked` mirrors the lock state for the idle adapter's
/// lock-wake policy and the core's drift re-assert; `is_active` mirrors the
/// session's foreground (`Active`) state for the core's active-session
/// arbitration gate.
pub fn spawn(client: SessionClient, is_locked: Arc<AtomicBool>, is_active: Arc<AtomicBool>) {
    tokio::spawn(run(client, is_locked, is_active));
}

/// Supervised watch loop: connect, hydrate + follow `LockedHint`/`Active`,
/// and run until the connection or the watched session ends; back off and
/// retry. Quiet after the first warning so a box without logind does not
/// spam.
async fn run(client: SessionClient, is_locked: Arc<AtomicBool>, is_active: Arc<AtomicBool>) {
    let mut backoff = Backoff::default();
    // Cleared once a watch has attached and seeded: the first attach seeds
    // silently, but a reconnect must fire a `LockedHint` change that
    // happened while the bus was down (see `watch`).
    let mut attached = false;
    loop {
        let outcome = match zbus::Connection::system().await {
            Ok(connection) => {
                watch(&connection, &client, &is_locked, &is_active, &mut attached).await
            }
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
    is_active: Arc<AtomicBool>,
) -> zbus::Result<()> {
    let mut attached = false;
    match watch(&connection, &client, &is_locked, &is_active, &mut attached).await {
        Ok(()) | Err(Fault::CoreGone) => Ok(()),
        Err(Fault::Bus(e)) => Err(e),
    }
}

/// One watch session: acquire this user's seat session, seed the lock
/// state, then forward `LockedHint` edges until the session goes away.
/// `attached` is `false` only for the process's first attach.
async fn watch(
    connection: &zbus::Connection,
    client: &SessionClient,
    is_locked: &AtomicBool,
    is_active: &AtomicBool,
    attached: &mut bool,
) -> Result<(), Fault> {
    let own_uid = own_uid();
    let manager = Login1ManagerProxy::new(connection).await?;
    let mut gone_stream = manager.receive_session_removed().await?;

    // Acquire this user's seat session: hydrate from the live list, else
    // wait for one to appear (the service can start before the session).
    let (session_id, session) = acquire_session(connection, &manager, own_uid).await?;

    // Seed the lock state. The first attach seeds silently: the initial
    // state is not an edge, and a synthesized Lock would re-blank a TV the
    // daemon already parked. A reconnect is different — the flag persists
    // across the bus-down gap, so a `LockedHint` change that happened while
    // disconnected is a real edge and must fire.
    if let Ok(locked) = session.locked_hint().await {
        let prev = is_locked.swap(locked, Ordering::SeqCst);
        if *attached && prev != locked {
            let event = if locked {
                SessionEvent::Lock
            } else {
                SessionEvent::Unlock
            };
            if !client.send(event) {
                return Err(Fault::CoreGone);
            }
        }
    }
    *attached = true;
    // Seed the foreground state for active-session arbitration. Unlike the
    // lock seed this fires no event and always stores the current value —
    // `Active` gates the core's dispatch, it does not itself dispatch.
    if let Ok(active) = session.active().await {
        is_active.store(active, Ordering::SeqCst);
    }
    let mut locked_changes = session.receive_locked_hint_changed().await;
    let mut active_changes = session.receive_active_changed().await;

    loop {
        tokio::select! {
            change = active_changes.next() => {
                // Session gone: reconnect and re-hydrate.
                let Some(change) = change else { return Ok(()) };
                let Ok(active) = change.get().await else { continue };
                is_active.store(active, Ordering::SeqCst);
            }
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

/// Return this user's seat session as `(id, proxy)`, hydrating from
/// `ListSessions` first (foreground session preferred), then waiting on
/// `SessionNew` until one qualifies. The `SessionNew` subscription is taken
/// before the list so a session appearing during hydration is not missed.
async fn acquire_session(
    connection: &zbus::Connection,
    manager: &Login1ManagerProxy<'_>,
    own_uid: u32,
) -> Result<(String, Login1SessionProxy<'static>), Fault> {
    let mut new_stream = manager.receive_session_new().await?;
    // A `ListSessions` fault must reconnect, not be swallowed into an empty
    // list — that would fall through to the `SessionNew` wait, which never
    // fires when the session already exists (a permanent silent hang).
    if let Some(found) =
        scan_sessions_for_own_seat(connection, manager.list_sessions().await?, own_uid).await
    {
        return Ok(found);
    }
    // None yet — this user's session may start after the service.
    loop {
        let Some(signal) = new_stream.next().await else {
            // Manager signal stream ended — treat as a bus drop to reconnect.
            return Err(Fault::Bus(zbus::Error::InvalidReply));
        };
        let Ok(args) = signal.args() else { continue };
        if let Some(session) = own_seat_session(connection, args.path(), own_uid).await {
            return Ok((args.id().to_string(), session));
        }
    }
}

/// Scan a `ListSessions` result for this user's seat session, preferring the
/// seat's foreground (`Active`) one and keeping the first seated match as a
/// fallback for a login whose active flag is transiently unset. Shared by
/// [`acquire_session`] (which waits for one to appear if none is listed) and
/// the startup [`seed_lock_state`] (which does not).
async fn scan_sessions_for_own_seat(
    connection: &zbus::Connection,
    sessions: Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)>,
    own_uid: u32,
) -> Option<(String, Login1SessionProxy<'static>)> {
    let mut fallback: Option<(String, Login1SessionProxy<'static>)> = None;
    for (id, uid, _user, seat, path) in sessions {
        if uid != own_uid || seat.is_empty() {
            continue;
        }
        let Some(session) = build_session(connection, &path).await else {
            continue;
        };
        if session.active().await.unwrap_or(false) {
            return Some((id, session));
        }
        fallback.get_or_insert((id, session));
    }
    fallback
}

/// Best-effort one-shot seed of `is_locked`/`is_active` from logind at
/// startup, to be awaited before the idle adapter is spawned.
///
/// A session that boots already locked must be observed as locked *here*: the
/// locked-idle `ext-idle-notify` transition fires once, one timeout in, and is
/// silently dropped if `is_locked` is still at its `false` default when it
/// arrives (and the later unlocked-idle transition is then dropped too, since
/// the two tags gate on opposite lock states). Unlike the supervised watcher
/// this never waits for a session to appear — if none is listed yet, the flags
/// keep their defaults and [`spawn`]'s watcher seeds them once one shows up.
/// Any bus/logind error leaves the defaults in place, silently: a host
/// without logind is a supported configuration.
pub async fn seed_lock_state(is_locked: &AtomicBool, is_active: &AtomicBool) {
    let Ok(connection) = zbus::Connection::system().await else {
        return;
    };
    seed_lock_state_on(&connection, is_locked, is_active).await;
}

/// One seed pass over an injected `connection` — the entry integration tests
/// drive against a private bus; production goes through [`seed_lock_state`].
/// Best-effort throughout: any missing session or bus/logind error leaves the
/// flags at their defaults.
pub async fn seed_lock_state_on(
    connection: &zbus::Connection,
    is_locked: &AtomicBool,
    is_active: &AtomicBool,
) {
    let Ok(manager) = Login1ManagerProxy::new(connection).await else {
        return;
    };
    let Ok(sessions) = manager.list_sessions().await else {
        return;
    };
    let Some((_id, session)) = scan_sessions_for_own_seat(connection, sessions, own_uid()).await
    else {
        return;
    };
    if let Ok(locked) = session.locked_hint().await {
        is_locked.store(locked, Ordering::SeqCst);
    }
    if let Ok(active) = session.active().await {
        is_active.store(active, Ordering::SeqCst);
    }
}

/// Build a `Session` proxy for `path`.
async fn build_session(
    connection: &zbus::Connection,
    path: &zbus::zvariant::OwnedObjectPath,
) -> Option<Login1SessionProxy<'static>> {
    Login1SessionProxy::builder(connection)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()
}

/// A `Session` proxy for `path` iff it is this user's seat session.
/// `SessionNew` carries neither uid nor seat, so both are read off the
/// object — the own-uid check the `ListSessions` tuple gives for free.
async fn own_seat_session(
    connection: &zbus::Connection,
    path: &zbus::zvariant::OwnedObjectPath,
    own_uid: u32,
) -> Option<Login1SessionProxy<'static>> {
    let session = build_session(connection, path).await?;
    if session.user().await.ok()?.0 != own_uid {
        return None;
    }
    (!session.seat().await.ok()?.0.is_empty()).then_some(session)
}
