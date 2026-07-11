//! logind Remote-session detector: the `Logind` remote source.
//!
//! xrdp-class stacks mint a real logind session with `Remote=yes` (krdp,
//! RustDesk and Sunshine are logind-invisible, so this covers the RDP
//! server family the portal monitor does not). The detector watches
//! `org.freedesktop.login1` for `SessionNew`/`SessionRemoved`, keeps the
//! ones that are both remote and owned by this service's own uid, and
//! reports a `Logind` source per such session — detail is the remote host
//! if the session carries one, else the session id.
//!
//! Bootstrap: `ListSessions` at startup re-arms a session that predates
//! the service (a restart inside a live RDP session). Degradation: no
//! system bus (headless CI) warns once and idles with supervised retry;
//! it never blocks or kills the session service.

use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_wire::payload::RemoteSourceKind;
use tracing::{debug, warn};

use crate::{RemoteClient, RemoteReport};

/// Backoff bounds for the supervised watch loop.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    /// `(session_id, object_path)` of a session as it appears.
    #[zbus(signal)]
    fn session_new(&self, id: String, path: zbus::zvariant::OwnedObjectPath) -> zbus::Result<()>;

    /// `(session_id, object_path)` of a session as it goes away.
    #[zbus(signal)]
    fn session_removed(
        &self,
        id: String,
        path: zbus::zvariant::OwnedObjectPath,
    ) -> zbus::Result<()>;

    /// `(id, uid, user_name, seat, object_path)` for each live session —
    /// the startup hydration source.
    fn list_sessions(
        &self,
    ) -> zbus::Result<Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn remote_host(&self) -> zbus::Result<String>;

    /// `(uid, object_path)` of the session's owner.
    #[zbus(property)]
    fn user(&self) -> zbus::Result<(u32, zbus::zvariant::OwnedObjectPath)>;

    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
}

/// The session facts the filter reads, lifted into an owned struct so the
/// translation is a pure fn (no live bus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    pub id: String,
    /// The owning uid — compared against the service's own uid.
    pub uid: u32,
    pub remote: bool,
    /// The client host, empty when the session carries none.
    pub remote_host: String,
}

/// Whether a session is a remote source for `own_uid`: it must be a remote
/// session owned by this user. A remote session under a *different* uid is
/// someone else's and never arms this user's TVs.
pub(crate) fn is_own_remote(info: &SessionInfo, own_uid: u32) -> bool {
    info.remote && info.uid == own_uid
}

/// The source detail for a qualifying session: the remote host when the
/// session names one, else the session id (always present) so the source
/// is still identifiable.
pub(crate) fn detail_for(info: &SessionInfo) -> String {
    if info.remote_host.is_empty() {
        format!("session {}", info.id)
    } else {
        info.remote_host.clone()
    }
}

/// The `Logind` report for a qualifying session, or `None` when it does
/// not arm. `active` reports the connect (`SessionNew`/bootstrap) edge;
/// the removal edge clears by the same detail.
pub(crate) fn report_for(info: &SessionInfo, own_uid: u32, active: bool) -> Option<RemoteReport> {
    is_own_remote(info, own_uid).then(|| RemoteReport {
        kind: RemoteSourceKind::Logind,
        detail: detail_for(info),
        active,
        provisional: false,
    })
}

/// Spawn the logind detector. Reports flow through `client` into the
/// session core; the task supervises its own system-bus connection and
/// never blocks the caller.
pub fn spawn(client: RemoteClient) {
    tokio::spawn(run(client));
}

/// Supervised watch loop: connect to the system bus, hydrate + watch, and
/// run until the connection ends; back off and retry. Quiet after the
/// first warning so a box without logind does not spam the log.
async fn run(client: RemoteClient) {
    let mut backoff = RETRY_MIN;
    let mut warned = false;
    loop {
        match watch(&client).await {
            Ok(()) => {
                debug!("logind detector: connection ended; reconnecting");
                backoff = RETRY_MIN;
            }
            Err(Fault::CoreGone) => return,
            Err(Fault::Bus(e)) => {
                if !warned {
                    warn!(%e, "logind detector: system bus unavailable; retrying quietly");
                    warned = true;
                } else {
                    debug!(%e, "logind detector: connect failed");
                }
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RETRY_MAX);
    }
}

/// Why a watch session ended.
enum Fault {
    Bus(zbus::Error),
    /// The session core dropped the report channel — stop supervising.
    CoreGone,
}

impl From<zbus::Error> for Fault {
    fn from(e: zbus::Error) -> Self {
        Fault::Bus(e)
    }
}

/// One watch session: hydrate the live sessions, then follow
/// `SessionNew`/`SessionRemoved` until either stream ends.
async fn watch(client: &RemoteClient) -> Result<(), Fault> {
    let own_uid = own_uid();
    let connection = zbus::Connection::system().await?;
    let manager = Login1ManagerProxy::new(&connection).await?;
    let mut new_stream = manager.receive_session_new().await?;
    let mut gone_stream = manager.receive_session_removed().await?;

    // Hydrate: a session that predates this service (a restart inside a
    // live RDP session) is re-armed from the current list.
    for (id, uid, _user, _seat, path) in manager.list_sessions().await.unwrap_or_default() {
        if let Some(info) = read_session(&connection, &id, uid, &path).await
            && let Some(report) = report_for(&info, own_uid, true)
        {
            emit(client, report)?;
        }
    }

    loop {
        tokio::select! {
            signal = new_stream.next() => {
                let Some(signal) = signal else { return Ok(()) };
                let Ok(args) = signal.args() else { continue };
                // `SessionNew` carries only id + path; the Remote/uid facts
                // are properties read off the object.
                if let Some(info) =
                    read_session(&connection, args.id(), u32::MAX, args.path()).await
                    && let Some(report) = report_for(&info, own_uid, true)
                {
                    emit(client, report)?;
                }
            }
            signal = gone_stream.next() => {
                let Some(signal) = signal else { return Ok(()) };
                let Ok(args) = signal.args() else { continue };
                // The object is gone by removal time, so its properties are
                // unreadable — reconstruct the same detail the connect edge
                // used to clear the matching source. A removed session with
                // no cached facts falls back to the id detail; a source
                // that was never armed clears to a no-op in the aggregator.
                let report = RemoteReport {
                    kind: RemoteSourceKind::Logind,
                    detail: format!("session {}", args.id()),
                    active: false,
                    provisional: false,
                };
                emit(client, report)?;
            }
        }
    }
}

/// Read the arming-relevant properties off a session object. `hint_uid`
/// short-circuits the `User` read when the caller already knows the uid
/// (the `ListSessions` hydration path); `u32::MAX` means "read it".
async fn read_session(
    connection: &zbus::Connection,
    id: &str,
    hint_uid: u32,
    path: &zbus::zvariant::OwnedObjectPath,
) -> Option<SessionInfo> {
    let session = Login1SessionProxy::builder(connection)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    // A non-remote session is the common case (the local seat) — read
    // `Remote` first and skip the rest when it is false.
    let remote = session.remote().await.ok()?;
    if !remote {
        return Some(SessionInfo {
            id: id.to_string(),
            uid: hint_uid,
            remote: false,
            remote_host: String::new(),
        });
    }
    let uid = if hint_uid == u32::MAX {
        session.user().await.ok()?.0
    } else {
        hint_uid
    };
    let remote_host = session.remote_host().await.unwrap_or_default();
    Some(SessionInfo {
        id: id.to_string(),
        uid,
        remote,
        remote_host,
    })
}

/// Forward a report to the core; a dead core loop stops the supervisor.
fn emit(client: &RemoteClient, report: RemoteReport) -> Result<(), Fault> {
    if client.report(report) {
        Ok(())
    } else {
        Err(Fault::CoreGone)
    }
}

/// The service's own uid — the session-ownership filter.
fn own_uid() -> u32 {
    // SAFETY: `getuid` is always successful and has no preconditions.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(uid: u32, remote: bool, host: &str) -> SessionInfo {
        SessionInfo {
            id: "7".into(),
            uid,
            remote,
            remote_host: host.into(),
        }
    }

    #[test]
    fn only_own_uid_remote_sessions_arm() {
        // Remote + own uid: arms.
        assert!(is_own_remote(&info(1000, true, "10.0.0.5"), 1000));
        // Remote but someone else's uid: never arms this user's TVs.
        assert!(!is_own_remote(&info(1001, true, "10.0.0.5"), 1000));
        // Own uid but a local (non-remote) session: not a remote source.
        assert!(!is_own_remote(&info(1000, false, ""), 1000));
    }

    #[test]
    fn detail_prefers_remote_host_then_falls_back_to_id() {
        assert_eq!(detail_for(&info(1000, true, "10.0.0.5")), "10.0.0.5");
        // No host recorded: the session id keeps the source identifiable.
        assert_eq!(detail_for(&info(1000, true, "")), "session 7");
    }

    #[test]
    fn report_arms_only_qualifying_sessions() {
        // A qualifying session yields an attributed active report.
        let report = report_for(&info(1000, true, "host.lan"), 1000, true).unwrap();
        assert_eq!(report.kind, RemoteSourceKind::Logind);
        assert_eq!(report.detail, "host.lan");
        assert!(report.active);
        assert!(!report.provisional, "logind sources always arm");
        // The disarm edge keeps the same detail so it clears the match.
        assert!(
            !report_for(&info(1000, true, "host.lan"), 1000, false)
                .unwrap()
                .active
        );
        // A non-qualifying session yields nothing.
        assert!(report_for(&info(1001, true, "host.lan"), 1000, true).is_none());
        assert!(report_for(&info(1000, false, ""), 1000, true).is_none());
    }
}
