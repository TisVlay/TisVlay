//! Key-expression builders. One function per key family so the literal
//! shape exists in exactly one place.

use uuid::Uuid;

/// Liveliness token declared by a running daemon for `site`.
pub fn daemon_alive(site: &str) -> String {
    format!("tisvlay/{site}/alive/daemon")
}

/// Liveliness token a running session service declares on its system
/// link — the daemon's crash-robust presence signal for the session.
/// A connect-only peer cannot serve entities to *other* leaves through
/// the hub, but the directly-dialed daemon does observe this token, and
/// its DELETE fires within a millisecond of the session's departure
/// (clean exit or `kill -9`). Distinct from the user-socket
/// [`user_alive_session`] token, which never reaches the system peer.
pub fn session_alive(site: &str) -> String {
    format!("tisvlay/{site}/alive/session")
}

/// A per-TV key: `tisvlay/<site>/tv/<uuid>/<path>` where `path` is a
/// [`StatePayload::PATH`](crate::payload::StatePayload::PATH) tail.
pub fn tv_path(site: &str, tv: &Uuid, path: &str) -> String {
    format!("tisvlay/{site}/tv/{tv}/{path}")
}

/// Observability tap: `tisvlay/<site>/event/<name>`.
pub fn event(site: &str, name: &str) -> String {
    format!("tisvlay/{site}/event/{name}")
}

/// Session-event tap published by the session service on the system
/// socket: `tisvlay/<site>/event/session/<name>`.
pub fn session_event(site: &str, name: &str) -> String {
    format!("tisvlay/{site}/event/session/{name}")
}

/// Greeter idle transitions, published by `tisvlay-greeter`:
/// `tisvlay/<site>/greeter/idle` with `{idle: bool}`.
pub fn greeter_idle(site: &str) -> String {
    format!("tisvlay/{site}/greeter/idle")
}

/// The daemon-computed greeter idle timeout (shortest enabled across
/// automated TVs, `{seconds}`; 0 = disabled): cached pub the greeter
/// subscribes to.
pub fn greeter_timeout(site: &str) -> String {
    format!("tisvlay/{site}/greeter/timeout")
}

/// User-profile body on the user socket:
/// `tisvlay/<site>/user/profile/<uuid>`.
pub fn user_profile(site: &str, id: &Uuid) -> String {
    format!("tisvlay/{site}/user/profile/{id}")
}

/// Per-TV user-profile assignment on the user socket.
pub fn user_tv_profile(site: &str, tv: &Uuid) -> String {
    format!("tisvlay/{site}/user/tv/{tv}/profile")
}

/// User-profile RPC key + the wildcard its queryable declares.
pub fn user_profile_rpc(site: &str, verb: &str) -> String {
    format!("tisvlay/{site}/user/profile/rpc/{verb}")
}

pub fn user_profile_rpc_wild(site: &str) -> String {
    format!("tisvlay/{site}/user/profile/rpc/*")
}

pub fn parse_user_profile_rpc<'a>(site: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/user/profile/rpc/")
}

/// Liveliness token for a running session service (user socket).
pub fn user_alive_session(site: &str) -> String {
    format!("tisvlay/{site}/user/alive/session")
}

/// The tray's notification settings (user socket, cached pub).
pub fn user_tray_settings(site: &str) -> String {
    format!("tisvlay/{site}/user/tray/settings")
}

/// Tray settings rpcs + the wildcard their queryable declares.
pub fn user_tray_rpc(site: &str, verb: &str) -> String {
    format!("tisvlay/{site}/user/tray/rpc/{verb}")
}

pub fn user_tray_rpc_wild(site: &str) -> String {
    format!("tisvlay/{site}/user/tray/rpc/*")
}

pub fn parse_user_tray_rpc<'a>(site: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/user/tray/rpc/")
}

/// Liveliness token for a running tray settings service.
pub fn user_alive_tray(site: &str) -> String {
    format!("tisvlay/{site}/user/alive/tray")
}

/// The per-user remote-session fact (user socket, cached pub owned
/// by the session's router).
pub fn user_remote_session(site: &str) -> String {
    format!("tisvlay/{site}/user/remote/session")
}

/// The system-socket mirror of the remote-session fact — a cache-1
/// AdvancedPublisher the session serves on its connect-only-peer system
/// link. A connect-only peer cannot fan a served entity out to *other*
/// leaves, but the directly-dialed daemon does hydrate its `@adv` cache,
/// so a daemon (re)start relearns the armed set without any re-put. The
/// counter-sequenced cache shape constructs without an HLC, so this
/// needs no daemon-side timestamping.
pub fn remote_session(site: &str) -> String {
    format!("tisvlay/{site}/remote/session")
}

/// Remote-session rpcs + the wildcard their queryable declares (own
/// family, not folded into the profile rpc queryable).
pub fn user_remote_rpc(site: &str, verb: &str) -> String {
    format!("tisvlay/{site}/user/remote/rpc/{verb}")
}

pub fn user_remote_rpc_wild(site: &str) -> String {
    format!("tisvlay/{site}/user/remote/rpc/*")
}

pub fn parse_user_remote_rpc<'a>(site: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/user/remote/rpc/")
}

/// Everything under the user subtree — must never be visible through
/// the system socket (gateway isolation invariant).
pub fn user_all(site: &str) -> String {
    format!("tisvlay/{site}/user/**")
}

/// Every TV's `meta/config` key — the session service's fleet mirror.
pub fn tv_meta_wild(site: &str) -> String {
    format!("tisvlay/{site}/tv/*/meta/config")
}

/// Every TV's `state/power` key — the session service's screen
/// mirror (idle drift re-assert).
pub fn tv_power_wild(site: &str) -> String {
    format!("tisvlay/{site}/tv/*/state/power")
}

/// All system-profile bodies.
pub fn profile_system_wild(site: &str) -> String {
    format!("tisvlay/{site}/profile/system/*")
}

pub fn parse_profile_system(site: &str, key: &str) -> Option<Uuid> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/profile/system/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn parse_user_profile(site: &str, key: &str) -> Option<Uuid> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/user/profile/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

pub fn parse_user_tv_profile(site: &str, key: &str) -> Option<Uuid> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/user/tv/")?
        .strip_suffix("/profile")
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// Pairing progress stream: `tisvlay/<site>/pairing/<uuid>/progress`.
pub fn pairing_progress(site: &str, tv: &Uuid) -> String {
    format!("tisvlay/{site}/pairing/{tv}/progress")
}

pub fn pairing_progress_wild(site: &str) -> String {
    format!("tisvlay/{site}/pairing/*/progress")
}

/// System-profile body: `tisvlay/<site>/profile/system/<uuid>`.
pub fn profile_system(site: &str, id: &Uuid) -> String {
    format!("tisvlay/{site}/profile/system/{id}")
}

/// Profile RPC key + the wildcard its queryable declares.
pub fn profile_rpc(site: &str, verb: &str) -> String {
    format!("tisvlay/{site}/profile/rpc/{verb}")
}

pub fn profile_rpc_wild(site: &str) -> String {
    format!("tisvlay/{site}/profile/rpc/*")
}

pub fn parse_profile_rpc<'a>(site: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/profile/rpc/")
}

/// Vendor catalog queryables: `manager/vendors` and
/// `manager/vendors/<id>/schema`.
pub fn vendors(site: &str) -> String {
    format!("tisvlay/{site}/manager/vendors")
}

pub fn vendor_schema(site: &str, vendor: &str) -> String {
    format!("tisvlay/{site}/manager/vendors/{vendor}/schema")
}

pub fn vendors_wild(site: &str) -> String {
    format!("tisvlay/{site}/manager/vendors/**")
}

/// A discovery session's finding key. Finding ids are arbitrary
/// vendor strings — CEC uses the device path (`/dev/cec0`), SSDP a
/// USN — so the id is sanitized into one valid key chunk (a `/`
/// would otherwise split it and leave an empty chunk, which zenoh
/// rejects and which silently dropped every CEC finding from the
/// wizard). Consumers identify findings by the payload's `id`
/// field; the chunk only needs validity and per-id stability.
pub fn discovery_found(site: &str, session: &Uuid, finding: &str) -> String {
    let mut chunk: String = finding
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if chunk.is_empty() {
        chunk.push('_');
    }
    format!("tisvlay/{site}/discovery/{session}/found/{chunk}")
}

/// Every discovery session's findings.
pub fn discovery_found_wild(site: &str) -> String {
    format!("tisvlay/{site}/discovery/*/found/*")
}

/// Liveliness token for a running discovery session.
pub fn alive_discovery(site: &str, session: &Uuid) -> String {
    format!("tisvlay/{site}/alive/discovery/{session}")
}

/// Liveliness token for a running pairing session.
pub fn alive_pairing(site: &str, tv: &Uuid) -> String {
    format!("tisvlay/{site}/alive/pairing/{tv}")
}

/// Manager RPC key: `tisvlay/<site>/manager/rpc/<verb>`.
pub fn manager_rpc(site: &str, verb: &str) -> String {
    format!("tisvlay/{site}/manager/rpc/{verb}")
}

/// Per-TV RPC key: `tisvlay/<site>/tv/<uuid>/rpc/<verb>`.
pub fn tv_rpc(site: &str, tv: &Uuid, verb: &str) -> String {
    format!("tisvlay/{site}/tv/{tv}/rpc/{verb}")
}

/// Wildcard the daemon's manager-RPC queryable is declared on.
pub fn manager_rpc_wild(site: &str) -> String {
    format!("tisvlay/{site}/manager/rpc/*")
}

/// Wildcard the daemon's per-TV-RPC queryable is declared on.
pub fn tv_rpc_wild(site: &str) -> String {
    format!("tisvlay/{site}/tv/*/rpc/*")
}

/// Every per-TV data key — client hydration subscriptions.
pub fn tv_all(site: &str) -> String {
    format!("tisvlay/{site}/tv/**")
}

/// Every data key for a site (`@adv` protocol chunks are verbatim and
/// deliberately not matched by `**`).
pub fn all(site: &str) -> String {
    format!("tisvlay/{site}/**")
}

/// Parse `tisvlay/<site>/tv/<uuid>/<path…>` into `(uuid, path)`.
pub fn parse_tv_path<'a>(site: &str, key: &'a str) -> Option<(Uuid, &'a str)> {
    let rest = key
        .strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/tv/")?;
    let (id, path) = rest.split_once('/')?;
    Some((Uuid::parse_str(id).ok()?, path))
}

/// Parse a per-TV RPC key into `(uuid, verb)`.
pub fn parse_tv_rpc<'a>(site: &str, key: &'a str) -> Option<(Uuid, &'a str)> {
    let (id, path) = parse_tv_path(site, key)?;
    Some((id, path.strip_prefix("rpc/")?))
}

/// Parse a manager RPC key into its verb.
pub fn parse_manager_rpc<'a>(site: &str, key: &'a str) -> Option<&'a str> {
    key.strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/manager/rpc/")
}

/// Parse a pairing-progress key into the pairing uuid.
pub fn parse_pairing_progress(site: &str, key: &str) -> Option<Uuid> {
    let rest = key
        .strip_prefix("tisvlay/")?
        .strip_prefix(site)?
        .strip_prefix("/pairing/")?
        .strip_suffix("/progress")?;
    Uuid::parse_str(rest).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_alive_key_shape() {
        assert_eq!(daemon_alive("main"), "tisvlay/main/alive/daemon");
    }

    #[test]
    fn session_alive_key_shape() {
        // System-namespace presence token (daemon-observable), distinct
        // from the user-socket session token the daemon cannot see.
        assert_eq!(session_alive("main"), "tisvlay/main/alive/session");
        assert_ne!(session_alive("main"), user_alive_session("main"));
    }

    #[test]
    fn discovery_found_sanitizes_arbitrary_finding_ids() {
        let session = Uuid::nil();
        // CEC's finding id is the device path — the raw form would
        // carry an empty chunk and be rejected by zenoh.
        let key = discovery_found("main", &session, "/dev/cec0");
        assert_eq!(
            key,
            "tisvlay/main/discovery/00000000-0000-0000-0000-000000000000/found/_dev_cec0"
        );
        for id in ["/dev/cec0", "uuid:5f2?a*b", "", "a b/c"] {
            let key = discovery_found("main", &session, id);
            assert!(
                zenoh::key_expr::KeyExpr::try_from(key.as_str()).is_ok(),
                "sanitized key must be a valid key expression: {key}"
            );
        }
    }

    #[test]
    fn tv_path_key_shape() {
        use crate::payload::StatePayload;
        let id = Uuid::nil();
        assert_eq!(
            tv_path("main", &id, crate::payload::Volume::PATH),
            "tisvlay/main/tv/00000000-0000-0000-0000-000000000000/state/volume"
        );
    }

    #[test]
    fn event_key_shape() {
        assert_eq!(
            event("main", "connect-failed"),
            "tisvlay/main/event/connect-failed"
        );
    }

    #[test]
    fn rpc_keys_round_trip_through_parsers() {
        let id = Uuid::nil();
        assert_eq!(
            parse_tv_rpc("main", &tv_rpc("main", &id, "set-volume")),
            Some((id, "set-volume"))
        );
        assert_eq!(
            parse_manager_rpc("main", &manager_rpc("main", "pair")),
            Some("pair")
        );
        assert_eq!(
            parse_pairing_progress("main", &pairing_progress("main", &id)),
            Some(id)
        );
        assert_eq!(
            parse_tv_path("main", &tv_path("main", &id, "state/volume")),
            Some((id, "state/volume"))
        );
    }

    #[test]
    fn parsers_reject_foreign_sites_and_shapes() {
        let id = Uuid::nil();
        assert_eq!(parse_tv_rpc("other", &tv_rpc("main", &id, "remove")), None);
        assert_eq!(parse_manager_rpc("main", "tisvlay/main/tv/x/rpc/y"), None);
        assert_eq!(
            parse_pairing_progress("main", "tisvlay/main/pairing/not-a-uuid/progress"),
            None
        );
    }

    #[test]
    fn remote_session_key_shapes() {
        assert_eq!(
            user_remote_session("main"),
            "tisvlay/main/user/remote/session"
        );
        assert_eq!(remote_session("main"), "tisvlay/main/remote/session");
        assert_eq!(
            parse_user_remote_rpc("main", &user_remote_rpc("main", "set")),
            Some("set")
        );
        // The wildcard the queryable declares matches its own keys.
        let wild = zenoh::key_expr::KeyExpr::try_from(user_remote_rpc_wild("main")).unwrap();
        let verb = zenoh::key_expr::KeyExpr::try_from(user_remote_rpc("main", "status")).unwrap();
        assert!(wild.intersects(&verb));
        // Wrong site is rejected; the tray family is not confused for it.
        assert_eq!(
            parse_user_remote_rpc("other", &user_remote_rpc("main", "set")),
            None
        );
        assert_eq!(
            parse_user_remote_rpc("main", &user_tray_rpc("main", "get")),
            None
        );
    }
}
