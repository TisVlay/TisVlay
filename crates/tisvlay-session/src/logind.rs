//! logind Remote-session detector: the `Logind` remote source.
//!
//! xrdp-class stacks mint a real logind session with `Remote=yes` (krdp,
//! RustDesk and Sunshine are logind-invisible, so this covers the RDP
//! server family the portal monitor does not). The detector watches
//! `org.freedesktop.login1` for `SessionNew`/`SessionRemoved`, keeps the
//! ones that are remote, graphical (`Type=x11`/`wayland` — a remote
//! *desktop*, not an SSH shell), and owned by this service's own uid, and
//! reports a `Logind` source per such session — detail is the remote host
//! if the session carries one, else the session id.
//!
//! Bootstrap: `ListSessions` at startup re-arms a session that predates
//! the service (a restart inside a live RDP session). Degradation: no
//! system bus (headless CI) warns once and idles with supervised retry;
//! it never blocks or kills the session service.

#![expect(
    unsafe_code,
    reason = "libc::getuid to find this user's logind sessions"
)]

use std::collections::HashMap;

use futures_util::StreamExt;
use tisvlay_wire::payload::RemoteSourceKind;
use tracing::debug;

use crate::detector::{Backoff, Fault};
use crate::{RemoteClient, RemoteReport};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
// Shared with the lock watcher (crate::lock), which reuses this identical
// Manager surface (ListSessions + SessionNew/Removed) rather than
// re-declaring it.
pub(crate) trait Login1Manager {
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

    /// The session's `Type`: `x11`/`wayland`/`mir` for graphical logins,
    /// `tty`/`unspecified` otherwise.
    #[zbus(property, name = "Type")]
    fn session_type(&self) -> zbus::Result<String>;

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
    /// The logind session `Type` — the remote-desktop discriminator.
    pub session_type: String,
}

/// Whether a session `Type` is a graphical (remote-desktop) login. Plain
/// SSH shells also mint `Remote=yes` sessions but are `Type=tty`; the
/// graphical type is what distinguishes a remote *desktop* from a remote
/// shell, and only a desktop drives the outputs this arms against. Shared
/// with the lock watcher, which wants the graphical seat session too.
pub(crate) fn is_graphical(session_type: &str) -> bool {
    matches!(session_type, "x11" | "wayland")
}

/// Whether a session is a remote source for `own_uid`: it must be a remote
/// graphical session owned by this user. A remote session under a
/// *different* uid is someone else's and never arms this user's TVs; a
/// non-graphical remote session (an SSH shell) never drives the display.
pub(crate) fn is_own_remote(info: &SessionInfo, own_uid: u32) -> bool {
    info.remote && info.uid == own_uid && is_graphical(&info.session_type)
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
/// first warning so a box without logind does not spam the log. The
/// reachable exits are a dead core (nothing left to report to) and, in
/// defense, a panic in `watch` — the latter strands this detector's
/// sources until a service restart re-hydrates, a design-pending residual
/// with no cross-process TTL to cover it.
async fn run(client: RemoteClient) {
    let mut backoff = Backoff::default();
    loop {
        match watch(&client).await {
            Ok(()) => {
                debug!("logind detector: connection ended; reconnecting");
                backoff.reset();
            }
            Err(Fault::CoreGone) => return,
            Err(Fault::Bus(e)) => backoff.note_failure("logind detector: system bus", e),
        }
        tokio::time::sleep(backoff.next_delay()).await;
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

    // Arm-time detail per session id: `SessionRemoved` cannot read the
    // gone object's properties, and the arm detail prefers the remote host
    // over the id, so the removal must clear by the *exact* detail the arm
    // used — reconstructing a bare `session <id>` key would leave a
    // host-bearing source armed forever. Rebuilt each connection; the
    // hydration below re-populates it after a reconnect.
    let mut armed_detail: HashMap<String, String> = HashMap::new();

    // Hydrate: a session that predates this service (a restart inside a
    // live RDP session) is re-armed from the current list.
    for (id, uid, _user, _seat, path) in manager.list_sessions().await.unwrap_or_default() {
        if let Some(info) = read_session(&connection, &id, uid, &path).await
            && let Some(report) = report_for(&info, own_uid, true)
        {
            armed_detail.insert(info.id.clone(), report.detail.clone());
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
                    armed_detail.insert(info.id.clone(), report.detail.clone());
                    emit(client, report)?;
                }
            }
            signal = gone_stream.next() => {
                let Some(signal) = signal else { return Ok(()) };
                let Ok(args) = signal.args() else { continue };
                emit(client, clear_for(&mut armed_detail, args.id()))?;
            }
        }
    }
}

/// The `Logind` clear report for a removed session id, keyed on the exact
/// detail the arm edge cached (the gone object's properties are
/// unreadable). Consumes the cache entry. An id that was never armed
/// clears by a bare `session <id>` detail — a no-op in the aggregator.
fn clear_for(armed_detail: &mut HashMap<String, String>, id: &str) -> RemoteReport {
    let detail = armed_detail
        .remove(id)
        .unwrap_or_else(|| format!("session {id}"));
    RemoteReport {
        kind: RemoteSourceKind::Logind,
        detail,
        active: false,
        provisional: false,
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
            session_type: String::new(),
        });
    }
    let uid = if hint_uid == u32::MAX {
        session.user().await.ok()?.0
    } else {
        hint_uid
    };
    let remote_host = session.remote_host().await.unwrap_or_default();
    let session_type = session.session_type().await.unwrap_or_default();
    Some(SessionInfo {
        id: id.to_string(),
        uid,
        remote,
        remote_host,
        session_type,
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

/// The service's own uid — the session-ownership filter. Shared with the
/// lock watcher, which scopes to this user's graphical session.
pub(crate) fn own_uid() -> u32 {
    // SAFETY: `getuid` is always successful and has no preconditions.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A remote graphical (wayland) session — the baseline qualifying shape.
    fn info(uid: u32, remote: bool, host: &str) -> SessionInfo {
        typed(uid, remote, host, "wayland")
    }

    fn typed(uid: u32, remote: bool, host: &str, session_type: &str) -> SessionInfo {
        SessionInfo {
            id: "7".into(),
            uid,
            remote,
            remote_host: host.into(),
            session_type: session_type.into(),
        }
    }

    #[test]
    fn only_own_uid_remote_sessions_arm() {
        // Remote graphical + own uid: arms.
        assert!(is_own_remote(&info(1000, true, "10.0.0.5"), 1000));
        // Remote but someone else's uid: never arms this user's TVs.
        assert!(!is_own_remote(&info(1001, true, "10.0.0.5"), 1000));
        // Own uid but a local (non-remote) session: not a remote source.
        assert!(!is_own_remote(&info(1000, false, ""), 1000));
    }

    #[test]
    fn only_graphical_remote_sessions_arm() {
        // x11 and wayland remote desktops arm.
        assert!(is_own_remote(&typed(1000, true, "10.0.0.5", "x11"), 1000));
        assert!(is_own_remote(
            &typed(1000, true, "10.0.0.5", "wayland"),
            1000
        ));
        // A plain SSH shell is Remote=yes too but Type=tty — never arms.
        assert!(!is_own_remote(&typed(1000, true, "10.0.0.5", "tty"), 1000));
        // A tty session that is not remote stays rejected.
        assert!(!is_own_remote(&typed(1000, false, "", "tty"), 1000));
        // A graphical session that is not remote (the local seat) stays
        // rejected — graphical alone is not a remote source.
        assert!(!is_own_remote(&typed(1000, false, "", "wayland"), 1000));
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
        // A remote SSH shell (Type=tty) yields nothing.
        assert!(report_for(&typed(1000, true, "host.lan", "tty"), 1000, true).is_none());
    }

    /// A host-bearing session arms under its host detail, so the removal
    /// must clear by that same host — not a reconstructed `session <id>`.
    /// Reconstruction left a host-keyed source armed forever (the removal
    /// never matched the arm key), so the TV stayed armed after disconnect.
    #[test]
    fn removal_clears_by_the_cached_arm_detail() {
        let info = info(1000, true, "10.0.0.5"); // id "7"
        let report = report_for(&info, 1000, true).unwrap();
        assert_eq!(report.detail, "10.0.0.5");

        let mut armed_detail = HashMap::new();
        armed_detail.insert(info.id.clone(), report.detail.clone());

        // The clear reports the armed detail (the host), so it matches the
        // aggregator key the arm inserted, and consumes the cache entry.
        let clear = clear_for(&mut armed_detail, &info.id);
        assert!(!clear.active);
        assert_eq!(clear.kind, RemoteSourceKind::Logind);
        assert_eq!(clear.detail, report.detail, "removal clears the armed key");
        assert!(armed_detail.is_empty(), "the cache entry is consumed");

        // A session never armed (no cache entry) clears by the bare id — a
        // harmless no-op against a source that was never live.
        let clear = clear_for(&mut armed_detail, "99");
        assert_eq!(clear.detail, "session 99");
    }
}
