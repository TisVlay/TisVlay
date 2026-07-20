//! Active-seat socket ACL: presence-scoped access for the human at the
//! console — `uaccess` semantics applied to the daemon socket instead of
//! a device node (`uaccess` itself is udev/device-node-only, so this has
//! to live in-daemon).
//!
//! The watch keys off each seat's `ActiveSession` property — never a scan
//! of sessions with `Active=yes`, because seatless sessions self-report
//! active (a plain SSH shell reads `Remote=yes Type=tty Active=yes`). The
//! active session qualifies only as a human desktop login: `Class=user`
//! (excludes the greeter session, which Workstream A's group entry
//! covers), `Type` x11/wayland (excludes a bare foreground VT), and not
//! `Remote`. The qualifying uids across all seats become named-user ACL
//! entries via [`acl::reconcile`].
//!
//! Transience is the point: switching away removes the entry, so new
//! connects are denied exactly while the session service suppresses that
//! user's automation (active-session arbitration); connections that
//! already exist persist, since the kernel checks permission at
//! `connect()` — the same property `uaccess` accepts for device fds. The
//! durable grant remains the `tisvlay` group.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::SelectAll;
use tracing::{debug, warn};
use zbus::zvariant::OwnedObjectPath;

use crate::acl::{self, GreeterAccess};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait SeatManager {
    /// `(seat_id, path)` for every seat logind knows.
    fn list_seats(&self) -> zbus::Result<Vec<(String, OwnedObjectPath)>>;

    #[zbus(signal)]
    fn seat_new(&self, id: String, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    fn seat_removed(&self, id: String, path: OwnedObjectPath) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Seat",
    default_service = "org.freedesktop.login1"
)]
trait Login1Seat {
    /// `(session_id, path)` of the seat's foreground session; the id is
    /// empty (path `/`) when nothing is active on the seat.
    #[zbus(property)]
    fn active_session(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait SeatSession {
    /// `(uid, path)` of the session owner.
    #[zbus(property)]
    fn user(&self) -> zbus::Result<(u32, OwnedObjectPath)>;

    /// `user` for a human login; the greeter session is `greeter`.
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;

    /// `x11` / `wayland` for a graphical login, `tty` for a VT.
    #[zbus(property, name = "Type")]
    fn session_type(&self) -> zbus::Result<String>;

    /// Whether the session came in over the network (SSH, xrdp).
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;
}

/// Belt-and-suspenders re-enumeration cadence: seat property streams can
/// die without a `SeatRemoved` (a logind restart re-creates objects), and
/// a stale named-user entry is an access grant, so the watch re-grounds
/// itself against logind periodically even when no event arrives.
const RESYNC_PERIOD: Duration = Duration::from_secs(300);

/// Supervised entry: run the seat watch over its own system-bus
/// connection, reconnecting with a fixed delay — a dead watch would leave
/// a stale user grant standing, so it must not stay dead on a bus blip.
/// Quiet after the first warning so a box without logind does not spam —
/// but a watch session that ran for a while was a *working* watch, so its
/// loss resets the latch: a fresh fault months into uptime warns again
/// instead of dying silently behind a warning spent at boot.
pub(crate) fn spawn(socket: PathBuf, greeter: GreeterAccess) {
    tokio::spawn(async move {
        let mut warned = false;
        loop {
            let started = tokio::time::Instant::now();
            let outcome = match zbus::Connection::system().await {
                Ok(connection) => watch_seats(connection, &socket, &greeter).await,
                Err(e) => Err(e),
            };
            if started.elapsed() > Duration::from_secs(60) {
                warned = false;
            }
            if let Err(e) = outcome
                && !std::mem::replace(&mut warned, true)
            {
                warn!(%e, "seat-access watch lost; retrying");
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

/// One watch session: enumerate seats, reconcile, then follow every
/// seat's `ActiveSession` property (plus seat add/remove and a periodic
/// resync) and reconcile on change. Returns only on a bus fault — a seat
/// proxy that can't be built is one (not an empty seat), surfaced so the
/// supervised retry re-grounds rather than reconciling with a seat
/// silently missing.
async fn watch_seats(
    connection: zbus::Connection,
    socket: &std::path::Path,
    greeter: &GreeterAccess,
) -> zbus::Result<()> {
    let manager = SeatManagerProxy::new(&connection).await?;
    let mut seat_new = manager.receive_seat_new().await?;
    let mut seat_removed = manager.receive_seat_removed().await?;
    loop {
        let seats = manager.list_seats().await?;
        let mut proxies = Vec::with_capacity(seats.len());
        let mut changes: SelectAll<_> = SelectAll::new();
        for (_id, path) in seats {
            let seat = Login1SeatProxy::builder(&connection)
                .path(path)?
                .build()
                .await?;
            changes.push(seat.receive_active_session_changed().await);
            proxies.push(seat);
        }
        reconcile_seats(&connection, &proxies, socket, greeter).await;
        // Follow this seat set until it changes shape or the resync
        // deadline arrives. The deadline is pinned *outside* the select so
        // property events can't reset it — it fires on wall-clock cadence,
        // which is what makes the stale-grant bound real on a busy host.
        let resync = tokio::time::sleep(RESYNC_PERIOD);
        tokio::pin!(resync);
        loop {
            tokio::select! {
                signal = seat_new.next() => {
                    if signal.is_none() {
                        return Err(zbus::Error::InvalidReply);
                    }
                    break;
                }
                signal = seat_removed.next() => {
                    if signal.is_none() {
                        return Err(zbus::Error::InvalidReply);
                    }
                    break;
                }
                Some(_) = changes.next(), if !changes.is_empty() => {
                    reconcile_seats(&connection, &proxies, socket, greeter).await;
                }
                () = &mut resync => break,
            }
        }
    }
}

/// Recompute the qualifying uid set across `seats` and reconcile the
/// socket ACL. No edge-detection here: `acl::reconcile` compares the
/// stored xattr against desired state and writes only on drift, so
/// property-stream echoes are no-ops while a failed or clobbered write
/// gets repaired at the resync cadence. A failed *seat* read aborts the
/// pass with state held — a transient bus fault must not read as "seat
/// empty" and strip a seated user's grant; the next event or resync
/// retries.
async fn reconcile_seats(
    connection: &zbus::Connection,
    seats: &[Login1SeatProxy<'_>],
    socket: &std::path::Path,
    greeter: &GreeterAccess,
) {
    let mut uids = BTreeSet::new();
    for seat in seats {
        let (_id, path) = match seat.active_session().await {
            Ok(active) => active,
            Err(e) => {
                debug!(%e, "seat read failed; holding the current socket ACL");
                return;
            }
        };
        if path.as_str() == "/" {
            continue;
        }
        if let Some(uid) = qualifying_uid(connection, &path).await {
            uids.insert(uid);
        }
    }
    acl::reconcile(socket, greeter, &uids);
}

/// The session's uid iff it is a local human desktop login: `Class=user`,
/// graphical `Type`, not `Remote`. Any read failure disqualifies — a
/// session that can't be classified must not be granted.
async fn qualifying_uid(connection: &zbus::Connection, path: &OwnedObjectPath) -> Option<u32> {
    let session = SeatSessionProxy::builder(connection)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    let qualifies = session.class().await.ok()? == "user"
        && matches!(
            session.session_type().await.ok()?.as_str(),
            "x11" | "wayland"
        )
        && !session.remote().await.ok()?;
    if !qualifies {
        return None;
    }
    let (uid, _) = session.user().await.ok()?;
    // Debug, not info: this runs on every recompute (property echoes
    // included); the applied-set edge is what acl::reconcile logs.
    debug!(uid, session = %path.as_str(), "active seat session qualifies for socket access");
    Some(uid)
}
