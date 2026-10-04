//! Shared wire vocabulary for the TisVlay zenoh stack.
//!
//! Everything both sides of the wire must agree on lives here: the key space,
//! the payload structs, and the session bootstrap (transport endpoint +
//! scouting discipline). Key-space reference: the "Key space" section of
//! `docs/architecture.md`.

pub mod endpoint;

pub mod payload {
    //! Wire payload structs — the load-bearing design decision:
    //! each struct is simultaneously the bevy component, the serde wire
    //! payload, and the value at one key ([`StatePayload::PATH`] under the
    //! TV's key prefix). Coupling this crate to `bevy_ecs` is intentional.

    use bevy_ecs::component::Component;
    use serde::{Deserialize, Serialize};

    /// A payload that lives at one per-TV key. `PATH` is the key tail under
    /// `tisvlay/<site>/tv/<uuid>/`.
    pub trait StatePayload: Component + Serialize + Clone + PartialEq {
        const PATH: &'static str;
    }

    /// Power state, projected from the vendor-reported substate.
    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct Power {
        pub state: tisvlay_core::PowerState,
        pub is_on: bool,
        pub is_screen_on: bool,
    }

    impl StatePayload for Power {
        const PATH: &'static str = "state/power";
    }

    impl From<&tisvlay_core::TvState> for Power {
        fn from(s: &tisvlay_core::TvState) -> Self {
            Self {
                state: s.power_state,
                is_on: s.is_on,
                is_screen_on: s.is_screen_on,
            }
        }
    }

    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Volume {
        pub level: u8,
        pub muted: bool,
    }

    impl StatePayload for Volume {
        const PATH: &'static str = "state/volume";
    }

    /// Connection health — derived from world structure (a TV with a live
    /// connection task is `connected`; one mid-dial is `reconnecting`).
    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Link {
        pub connected: bool,
        pub reconnecting: bool,
        pub supports_wake: bool,
    }

    impl StatePayload for Link {
        const PATH: &'static str = "state/link";
    }

    /// Current + available inputs.
    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Input {
        pub current: Option<String>,
        pub available: Vec<InputEntry>,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct InputEntry {
        pub id: String,
        pub label: String,
        pub connected: bool,
    }

    impl StatePayload for Input {
        const PATH: &'static str = "state/input";
    }

    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Backlight {
        pub level: u8,
    }

    impl StatePayload for Backlight {
        const PATH: &'static str = "state/backlight";
    }

    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Identity {
        pub name: String,
        pub vendor: String,
        /// Canonical device brand when the backend can resolve it.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub brand: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub make: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub serial: Option<String>,
        pub capabilities: Vec<String>,
        /// DRM connector this TV mirrors, once the desktop probe matches it.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub connector: Option<String>,
        /// The vendor's primary identifying descriptor (LG: IP; CEC:
        /// device node) — the connector's `primary_descriptor`, evaluated
        /// by the reactor from the vendor config. Clients render it as
        /// the TV's spatial hint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub primary: Option<String>,
    }

    impl Identity {
        /// A fresh identity carrying only what enrollment knows.
        pub fn new(name: impl Into<String>, vendor: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                vendor: vendor.into(),
                brand: None,
                make: None,
                model: None,
                serial: None,
                capabilities: Vec::new(),
                connector: None,
                primary: None,
            }
        }
    }

    impl StatePayload for Identity {
        const PATH: &'static str = "meta/identity";
    }

    /// Vendor-projected display rows for the TV's read-only INFO surface:
    /// the connector's `display_config` (editable fields) and
    /// `display_diagnostics` (pair-time captures) as schema-ordered,
    /// labeled `(label, value)` pairs, Secret-free. Arrays preserve order
    /// on the wire — JSON objects re-sort alphabetically — so this is the
    /// only shape row order survives in.
    #[derive(Component, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
    pub struct VendorRows {
        pub config: Vec<(String, String)>,
        pub diagnostics: Vec<(String, String)>,
    }

    impl StatePayload for VendorRows {
        const PATH: &'static str = "meta/vendor";
    }

    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Meta {
        pub managed: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub system_profile: Option<uuid::Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub primary_input: Option<String>,
        /// Read-only projection of the resolved profile-chain
        /// `automate_when` policy (assigned profile - default profile -
        /// always); the world's derive pass is the only writer.
        pub automate_when: String,
        /// The evaluated automation gate: `managed` AND the resolved
        /// `automate_when` holds against the current input. Consumers
        /// (session service, tray) act only on automated TVs.
        pub automated: bool,
        /// Whether a remote session currently arms this TV (mirrored
        /// from the session service). While set, the world honors the
        /// system profile's while-remote overrides on DPMS-on/resume; a
        /// read-only projection for clients to surface the state.
        #[serde(default)]
        pub remote_armed: bool,
    }

    impl Meta {
        pub fn new(managed: bool) -> Self {
            Self {
                managed,
                system_profile: None,
                primary_input: None,
                automate_when: "always".into(),
                // `should_automate` gives unknown inputs the benefit of
                // the doubt, so a fresh managed TV starts automated.
                automated: managed,
                // No remote session until the mirror says so.
                remote_armed: false,
            }
        }
    }

    impl StatePayload for Meta {
        const PATH: &'static str = "meta/config";
    }

    /// Every per-TV cached-state key tail — the DELETE sweep on TV removal.
    pub const TV_PATHS: [&str; 8] = [
        Power::PATH,
        Volume::PATH,
        Link::PATH,
        Input::PATH,
        Backlight::PATH,
        Identity::PATH,
        Meta::PATH,
        VendorRows::PATH,
    ];

    /// Non-payload per-TV keys included in the removal sweep.
    pub const TV_EXTRA_PATHS: [&str; 1] = ["progress/power"];

    /// Pairing progress on `tisvlay/<site>/pairing/<uuid>/progress` — the
    /// redacted projection of the core pairing state: credentials never
    /// reach the wire, clients branch on `phase` and localize by `code`.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct PairingProgress {
        /// The pairing attempt this sample belongs to, minted per pair /
        /// re-pair and returned by that rpc. A TV's progress key outlives
        /// its sessions — a history subscriber sees earlier attempts'
        /// samples replayed from the cache, in an order relative to the
        /// live ones that isn't guaranteed — so a watcher drains only its
        /// own attempt.
        pub attempt: uuid::Uuid,
        /// `awaiting_tv` | `awaiting_input` | `complete` | `failed`.
        pub phase: String,
        pub message: String,
        /// Namespaced failure reason (e.g. `pairing.rejected`,
        /// `device.permission-denied`), `failed` only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub code: Option<String>,
        /// Reason-specific parameter — the device-node path on a
        /// permission-denied failure, so clients can name the node in
        /// the fix hint. Absent for reasons that carry none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        /// `complete` only: what the device reported as its link MAC and
        /// what the daemon did with it. Absent when the vendor can't
        /// report one or the read failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub link_mac: Option<LinkMacReport>,
    }

    /// The device-reported link MAC at pairing time, compared against the
    /// TV's configured `mac` by the daemon so clients only render it. A
    /// snapshot of the moment the pairing completed — accepting the
    /// reported MAC afterwards doesn't rewrite this sample.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct LinkMacReport {
        /// The device-reported MAC, canonical form.
        pub reported: String,
        pub outcome: LinkMacOutcome,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum LinkMacOutcome {
        /// `mac` was unset; the daemon stored `reported`.
        Filled,
        /// The configured `mac` already equals `reported`.
        Matches,
        /// The configured `mac` differs and was kept.
        Differs { configured: String },
    }

    impl PairingProgress {
        /// Project a core pairing state onto `attempt`'s progress sample.
        pub fn from_state(attempt: uuid::Uuid, state: &tisvlay_core::PairingState) -> Self {
            Self {
                attempt,
                phase: state.to_dbus_state().into(),
                message: state.message().into(),
                code: state.error().map(|e| e.code.as_str().into()),
                // Handshake errors carry no device parameter; dial
                // failures (which do) are composed by the world from
                // the typed dial reason, not through this projection.
                path: None,
                link_mac: None,
            }
        }
    }

    /// A remote-session fact: every live source tagged by kind, plus the
    /// per-TV `armed` projection the session computes from the sources
    /// crossed with each TV's resolved user profile. The world (via the
    /// system mirror) consumes `armed` booleans only, staying
    /// arming-policy-free.
    #[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
    pub struct RemoteSessionState {
        pub sources: Vec<RemoteSource>,
        pub armed: Vec<uuid::Uuid>,
    }

    /// One live remote source. `detail` names the specific origin (the
    /// resolved exe for `Portal`, the source label for `Manual`); it's
    /// carried for display and attribution, not routing.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct RemoteSource {
        pub kind: RemoteSourceKind,
        pub detail: String,
    }

    /// How a remote source was detected. Attributed kinds always arm;
    /// the unattributed `Screencast` signal arms only when the resolved
    /// profile opts in (`remote_screencast_arms`). `Ord` so the source
    /// set keys deterministically.
    ///
    /// `Sunshine` is the continuous `/serverinfo` level poll's own key,
    /// kept distinct from the `Manual` hook verb so the poll and the
    /// optional `global_prep_cmd` hook coexist without racing each other's
    /// arm.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[serde(rename_all = "snake_case")]
    pub enum RemoteSourceKind {
        Manual,
        Logind,
        Portal,
        Screencast,
        Sunshine,
    }
}

pub mod keys {
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
            let verb =
                zenoh::key_expr::KeyExpr::try_from(user_remote_rpc("main", "status")).unwrap();
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
}

pub mod error {
    //! The rpc error contract: every `reply_err` payload is this shape,
    //! clients branch on `code` and show `message`.

    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    /// Typed failure reason for a command reply — serialized into the
    /// zenoh `reply_err` payload.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct WireError {
        pub code: String,
        pub message: String,
    }

    impl WireError {
        pub fn tv_not_found(id: Uuid) -> Self {
            Self {
                code: "tv-not-found".into(),
                message: format!("no TV with id {id}"),
            }
        }

        pub fn tv_exists(id: Uuid) -> Self {
            Self {
                code: "tv-exists".into(),
                message: format!("a TV with id {id} already exists"),
            }
        }

        pub fn not_connected(id: Uuid) -> Self {
            Self {
                code: "not-connected".into(),
                message: format!("TV {id} has no live connection"),
            }
        }

        pub fn unknown_vendor(vendor: &str) -> Self {
            Self {
                code: "unknown-vendor".into(),
                message: format!("no backend registered for vendor '{vendor}'"),
            }
        }

        pub fn pairing_exists(id: Uuid) -> Self {
            Self {
                code: "pairing-exists".into(),
                message: format!("a pairing session for {id} is already running"),
            }
        }

        pub fn pairing_not_found(id: Uuid) -> Self {
            Self {
                code: "pairing-not-found".into(),
                message: format!("no pairing session for {id}"),
            }
        }

        pub fn wake_not_supported(id: Uuid) -> Self {
            Self {
                code: "wake-not-supported".into(),
                message: format!("TV {id} is disconnected and its transport cannot wake it"),
            }
        }

        pub fn bad_request(message: impl Into<String>) -> Self {
            Self {
                code: "bad-request".into(),
                message: message.into(),
            }
        }

        /// A specific vendor-config field failed validation. The
        /// message's `"<field>: <reason>"` shape is load-bearing:
        /// clients route the reason back to the originating form row
        /// by splitting on the first `": "`.
        pub fn invalid_field(field: &str, reason: impl std::fmt::Display) -> Self {
            Self {
                code: "invalid-field".into(),
                message: format!("{field}: {reason}"),
            }
        }

        /// The reserved default profile cannot be deleted.
        pub fn profile_reserved(id: Uuid) -> Self {
            Self {
                code: "profile-reserved".into(),
                message: format!("profile {id} is the reserved default and cannot be deleted"),
            }
        }

        /// The backend accepted the command but the device call failed.
        pub fn command_failed(message: impl Into<String>) -> Self {
            Self {
                code: "command-failed".into(),
                message: message.into(),
            }
        }

        pub fn profile_not_found(id: Uuid) -> Self {
            Self {
                code: "profile-not-found".into(),
                message: format!("no system profile with id {id}"),
            }
        }

        pub fn profile_exists(id: Uuid) -> Self {
            Self {
                code: "profile-exists".into(),
                message: format!("a system profile with id {id} already exists"),
            }
        }

        pub fn profile_in_use(id: Uuid) -> Self {
            Self {
                code: "profile-in-use".into(),
                message: format!("system profile {id} is referenced by at least one TV"),
            }
        }

        pub fn unknown_rpc(verb: &str) -> Self {
            Self {
                code: "unknown-rpc".into(),
                message: format!("no such rpc: {verb}"),
            }
        }

        pub fn discovery_not_found(session: Uuid) -> Self {
            Self {
                code: "discovery-not-found".into(),
                message: format!("no discovery session {session}"),
            }
        }

        pub fn daemon_shutting_down() -> Self {
            Self {
                code: "daemon-shutting-down".into(),
                message: "world reactor is gone".into(),
            }
        }
    }
}

/// Request bodies for the RPC queryables — shared by the daemon bridge and
/// clients so the wire shape exists in exactly one place.
pub mod rpc {
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    fn default_true() -> bool {
        true
    }

    /// Body for `manager/rpc/add-tv` and `manager/rpc/pair`. The `config`
    /// is the vendor config (JSON on the wire, TOML at the backend seam).
    /// Omitted `id` means "daemon generates one"; the reply carries it.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct AddTv {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id: Option<Uuid>,
        pub name: String,
        pub vendor: String,
        #[serde(default = "default_true")]
        pub managed: bool,
        pub config: serde_json::Value,
    }

    /// Body for RPCs that only reference a TV / pairing session by id.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TvRef {
        pub id: Uuid,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SetVolume {
        pub level: u8,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SetMuted {
        pub muted: bool,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct SetName {
        pub name: String,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SetManaged {
        pub managed: bool,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct SetSystemProfile {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub profile: Option<Uuid>,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
    pub struct SetPrimaryInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub input: Option<String>,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct SwitchInput {
        pub input: String,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SetBacklight {
        pub level: u8,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SetScreen {
        pub on: bool,
        /// Resolved profile intent: land the TV on its `primary_input`
        /// once the action completes. Manual clients leave it false.
        #[serde(default)]
        pub restore_input: bool,
    }

    /// Body for `tv/<id>/rpc/power-on` — optional; an absent or null
    /// body is a plain manual power-on.
    #[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
    pub struct PowerOn {
        /// See [`SetScreen::restore_input`].
        #[serde(default)]
        pub restore_input: bool,
    }

    /// Body for `profile/rpc/create` (and boot replay): omitted `id`
    /// means "daemon generates one"; the reply carries it.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct ProfileCreate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id: Option<Uuid>,
        pub body: tisvlay_core::SystemProfileConfig,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct ProfileUpdate {
        pub id: Uuid,
        pub body: tisvlay_core::SystemProfileConfig,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct ProfileDuplicate {
        pub id: Uuid,
        pub name: String,
    }

    /// Bodies for the user-profile rpcs served by the session service on
    /// the user socket. Duplicate/delete/effective reuse [`ProfileDuplicate`]
    /// and [`TvRef`].
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct UserProfileCreate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id: Option<Uuid>,
        pub body: tisvlay_core::UserProfileConfig,
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct UserProfileUpdate {
        pub id: Uuid,
        pub body: tisvlay_core::UserProfileConfig,
    }

    /// Assign (or with `profile: null` unassign) a user profile to a TV.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct UserAssign {
        pub tv: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub profile: Option<Uuid>,
    }

    /// Body for `manager/rpc/discover-start`: an open-ended scan session
    /// publishing findings on `discovery/<session>/found/*` with a
    /// liveliness token until `discover-stop`.
    #[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
    pub struct DiscoverStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub vendor: Option<String>,
    }

    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DiscoverStop {
        pub session: Uuid,
    }

    /// Body for `manager/rpc/discover` — a bounded scan whose findings
    /// stream back as one query reply per discovered TV.
    #[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
    pub struct Discover {
        /// Restrict to one vendor; omitted scans every discovery-capable
        /// backend.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub vendor: Option<String>,
        /// Scan window in milliseconds (defaulted and clamped daemon-side
        /// to stay under the zenoh query-timeout ceiling).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64>,
    }

    /// Body for `manager/rpc/enrich`: hint a manually entered TV's MAC
    /// from the ARP cache before pairing.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Enrich {
        pub vendor: String,
        pub ip: String,
    }

    /// Body for `tv/<id>/rpc/update-config`: known top-level keys apply
    /// to the TV's settings, everything else merges into the vendor table.
    pub type UpdateConfig = serde_json::Map<String, serde_json::Value>;

    /// Body for `user/remote/rpc/set`: toggle a manual remote-session
    /// source (`manual` when omitted).
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct RemoteSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source: Option<String>,
        pub active: bool,
    }

    /// The tray's notification switches: the cached settings pub and the
    /// `get`/`update` replies (field names are the category keys).
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct NotificationSettings {
        pub tv_connection: bool,
        pub tv_pairing: bool,
        pub tv_wake: bool,
        pub daemon_offline: bool,
    }

    impl Default for NotificationSettings {
        fn default() -> Self {
            // Every category documented default-on.
            Self {
                tv_connection: true,
                tv_pairing: true,
                tv_wake: true,
                daemon_offline: true,
            }
        }
    }

    impl NotificationSettings {
        /// Apply a partial update: absent fields keep their current value.
        #[must_use]
        pub fn merged_with(self, patch: &NotificationSettingsPatch) -> Self {
            Self {
                tv_connection: patch.tv_connection.unwrap_or(self.tv_connection),
                tv_pairing: patch.tv_pairing.unwrap_or(self.tv_pairing),
                tv_wake: patch.tv_wake.unwrap_or(self.tv_wake),
                daemon_offline: patch.daemon_offline.unwrap_or(self.daemon_offline),
            }
        }
    }

    /// Body for `user/tray/rpc/update`: only the switches being changed.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct NotificationSettingsPatch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tv_connection: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tv_pairing: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tv_wake: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub daemon_offline: Option<bool>,
    }

    /// Reply to an rpc that only acknowledges: `{}` on the wire.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Ack {}

    /// Reply carrying the id the service minted (TV or profile creation).
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Created {
        pub id: Uuid,
    }

    /// Reply to `manager/rpc/pair`: the TV id and this session's attempt
    /// id, which stamps its progress samples.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PairStarted {
        pub id: Uuid,
        pub attempt: Uuid,
    }

    /// Reply to `tv/<id>/rpc/re-pair`: the new session's attempt id.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct RePairStarted {
        pub attempt: Uuid,
    }

    /// Reply to `manager/rpc/discover-start`: the scan session id.
    #[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DiscoverySession {
        pub session: Uuid,
    }

    /// Reply to every service's `info` rpc. `state_path` is empty when
    /// the service runs without persistence.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct ServiceInfo {
        pub version: String,
        pub state_path: String,
    }

    /// One entry of the `manager/vendors` catalog.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct VendorEntry {
        pub id: String,
        pub name: String,
        pub capabilities: Vec<String>,
    }

    /// Reply to `manager/rpc/enrich`: the MAC hint, when the ARP cache
    /// had one.
    #[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
    pub struct EnrichReply {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub mac: Option<String>,
    }

    /// Reply to `tv/<id>/rpc/get-config`: the TV's settings plus its
    /// vendor table with schema-`Secret` fields removed.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct TvConfigView {
        pub id: Uuid,
        pub name: String,
        pub vendor: String,
        pub managed: bool,
        pub system_profile: Option<Uuid>,
        pub primary_input: Option<String>,
        pub config: serde_json::Map<String, serde_json::Value>,
    }
}

pub mod caller {
    //! Matching-gated rpc calls — the client half of the querier-matching
    //! law (pinned in `tests/querier_matching.rs`). A [`Caller`] holds one
    //! declared querier per exact rpc key; every call awaits the querier's
    //! matching status before firing, so the zero-matching-queryables
    //! instant-no-reply race cannot happen: matching ⇒ routable. A
    //! service absent past the deadline fails fast as
    //! [`RpcError::Unavailable`], and the liveliness token stays a
    //! *presence display* signal only — rpc readiness is the querier's job.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use zenoh::Session;
    use zenoh::bytes::Encoding;
    use zenoh::query::{ConsolidationMode, Querier};

    use crate::endpoint::{Streaming, Unary};
    use crate::error::WireError;

    /// Per-call timeout once a matched queryable holds the query.
    pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);

    /// How long a call waits for a matching queryable to become routable
    /// before failing [`RpcError::Unavailable`]. Route propagation on a
    /// fresh link completes in tens of milliseconds; the margin covers a
    /// service that is mid-start.
    pub const MATCHING_DEADLINE: Duration = Duration::from_secs(2);

    /// How an rpc call ended, when it didn't end in a typed reply.
    #[derive(Debug)]
    pub enum RpcError {
        /// The service refused the request.
        Service(WireError),
        /// No service matched the key within [`MATCHING_DEADLINE`].
        Unavailable { key: String },
        /// A service held the query but didn't reply within [`RPC_TIMEOUT`].
        Timeout { key: String },
        /// The query closed without a reply after matching confirmed (the
        /// service dropped mid-call). For a write, the outcome is unknown.
        NoReply { key: String },
        /// The local zenoh session failed the call.
        Transport { key: String, source: zenoh::Error },
        /// A reply that isn't the rpc's response type, or an error payload
        /// that isn't a [`WireError`].
        Undecodable { key: String },
    }

    impl std::fmt::Display for RpcError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Service(error) => write!(f, "{} ({})", error.message, error.code),
                Self::Unavailable { key } => {
                    write!(f, "no rpc service for {key} within {MATCHING_DEADLINE:?}")
                }
                Self::Timeout { key } => write!(f, "rpc {key}: timed out"),
                Self::NoReply { key } => write!(f, "rpc {key}: no reply"),
                Self::Transport { key, .. } => write!(f, "rpc {key}: transport failure"),
                Self::Undecodable { key } => write!(f, "rpc {key}: undecodable reply"),
            }
        }
    }

    impl std::error::Error for RpcError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                Self::Transport { source, .. } => Some(source.as_ref()),
                _ => None,
            }
        }
    }

    /// Matching-gated rpc caller over one zenoh session, addressing one
    /// site. Queriers are cached per key, so repeat calls skip
    /// declaration and their matching status is already resolved.
    #[derive(Debug)]
    pub struct Caller {
        session: Session,
        site: String,
        queriers: Mutex<HashMap<String, Arc<Querier<'static>>>>,
    }

    impl Caller {
        pub fn new(session: Session, site: impl Into<String>) -> Self {
            Self {
                session,
                site: site.into(),
                queriers: Mutex::new(HashMap::new()),
            }
        }

        /// The site every call addresses.
        pub fn site(&self) -> &str {
            &self.site
        }

        /// One rpc, one typed reply. Matching-gated; re-issued once when
        /// the reply channel closes empty only if `R::REISSUE` (reads) —
        /// a write that minted state before a lost reply must surface as
        /// an error rather than mint twice.
        pub async fn call<R: Unary>(
            &self,
            target: &R::Target,
            request: &R::Request,
        ) -> Result<R::Response, RpcError> {
            let key = R::key(&self.site, target);
            let body = encode(request);
            let querier = self.querier(&key).await?;
            self.await_matching(&querier).await?;
            let bytes = match issue(&querier, &body).await {
                Err(RpcError::NoReply { .. }) if R::REISSUE => {
                    self.await_matching(&querier).await?;
                    issue(&querier, &body).await?
                }
                outcome => outcome?,
            };
            decode(&key, &bytes)
        }

        /// One rpc, one reply per finding, collected until `R::window`
        /// closes. Consolidation is off so same-key replies survive; zero
        /// replies is a legitimate empty result, never re-issued. The
        /// querier is declared per call since its timeout is the window.
        pub async fn call_many<R: Streaming>(
            &self,
            target: &R::Target,
            request: &R::Request,
        ) -> Result<Vec<R::Response>, RpcError> {
            let key = R::key(&self.site, target);
            let querier = self
                .session
                .declare_querier(key.clone())
                .timeout(R::window(request))
                .consolidation(ConsolidationMode::None)
                .await
                .map_err(|source| transport(&key, source))?;
            self.await_matching(&querier).await?;
            let replies = querier
                .get()
                .payload(encode(request))
                .encoding(Encoding::APPLICATION_JSON)
                .await
                .map_err(|source| transport(&key, source))?;
            let mut out = Vec::new();
            while let Ok(reply) = replies.recv_async().await {
                out.push(decode(&key, &reply_bytes(&key, reply)?)?);
            }
            Ok(out)
        }

        /// The cached single-reply querier for `key`.
        async fn querier(&self, key: &str) -> Result<Arc<Querier<'static>>, RpcError> {
            if let Some(querier) = self.queriers.lock().expect("querier cache lock").get(key) {
                return Ok(Arc::clone(querier));
            }
            let declared = Arc::new(
                self.session
                    .declare_querier(key.to_string())
                    .timeout(RPC_TIMEOUT)
                    .await
                    .map_err(|source| transport(key, source))?,
            );
            // A concurrent first call may have raced us; keep whichever
            // landed first so the loser's querier undeclares on drop.
            Ok(Arc::clone(
                self.queriers
                    .lock()
                    .expect("querier cache lock")
                    .entry(key.to_string())
                    .or_insert(declared),
            ))
        }

        /// Wait (bounded) until the querier has a matching routable
        /// queryable — the fact liveliness tokens cannot provide, since
        /// token arrival races queryable-route propagation.
        async fn await_matching(&self, querier: &Querier<'static>) -> Result<(), RpcError> {
            let key = querier.key_expr().as_str();
            if status(querier).await?.matching() {
                return Ok(());
            }
            let listener = querier
                .matching_listener()
                .await
                .map_err(|source| transport(key, source))?;
            // Re-check after subscribing: the flip may have landed
            // between the status read and the listener declaration.
            if status(querier).await?.matching() {
                return Ok(());
            }
            tokio::time::timeout(MATCHING_DEADLINE, async {
                while let Ok(status) = listener.recv_async().await {
                    if status.matching() {
                        return Ok(());
                    }
                }
                Err(transport(key, "matching listener closed".into()))
            })
            .await
            .map_err(|_| RpcError::Unavailable {
                key: key.to_string(),
            })?
        }
    }

    async fn status(
        querier: &Querier<'static>,
    ) -> Result<zenoh::matching::MatchingStatus, RpcError> {
        querier
            .matching_status()
            .await
            .map_err(|source| transport(querier.key_expr().as_str(), source))
    }

    /// Issue one query and take its first reply's payload.
    async fn issue(querier: &Querier<'static>, body: &[u8]) -> Result<Vec<u8>, RpcError> {
        let key = querier.key_expr().as_str().to_string();
        let replies = querier
            .get()
            .payload(body.to_vec())
            .encoding(Encoding::APPLICATION_JSON)
            .await
            .map_err(|source| transport(&key, source))?;
        match replies.recv_async().await {
            Ok(reply) => reply_bytes(&key, reply),
            // Empty channel close: the query finalized with zero replies.
            // With matching confirmed this is the rare dropped-mid-call
            // case, not the routing race.
            Err(_) => Err(RpcError::NoReply { key }),
        }
    }

    fn transport(key: &str, source: zenoh::Error) -> RpcError {
        RpcError::Transport {
            key: key.to_string(),
            source,
        }
    }

    fn encode<T: Serialize + ?Sized>(body: &T) -> Vec<u8> {
        serde_json::to_vec(body)
            .expect("rpc bodies are structs and string-keyed maps, which always encode as JSON")
    }

    fn decode<T: DeserializeOwned>(key: &str, bytes: &[u8]) -> Result<T, RpcError> {
        serde_json::from_slice(bytes).map_err(|_| RpcError::Undecodable {
            key: key.to_string(),
        })
    }

    /// A reply's payload, or the call's failure: the service's
    /// [`WireError`], or zenoh's synthetic timeout reply.
    fn reply_bytes(key: &str, reply: zenoh::query::Reply) -> Result<Vec<u8>, RpcError> {
        match reply.result() {
            Ok(sample) => Ok(sample.payload().to_bytes().into_owned()),
            Err(err) => {
                let bytes = err.payload().to_bytes();
                match serde_json::from_slice::<WireError>(&bytes) {
                    Ok(error) => Err(RpcError::Service(error)),
                    // Zenoh's query timeout arrives as a synthetic
                    // ReplyError("Timeout"), not a WireError — keep "service
                    // slow" distinguishable from a genuine service error.
                    Err(_) if String::from_utf8_lossy(&bytes).contains("Timeout") => {
                        Err(RpcError::Timeout {
                            key: key.to_string(),
                        })
                    }
                    Err(_) => Err(RpcError::Undecodable {
                        key: key.to_string(),
                    }),
                }
            }
        }
    }
}

pub mod bootstrap {
    //! Session configuration for daemon (peer + UDS listener) and clients
    //! (connect-only). Multicast and gossip scouting are pinned off in both:
    //! the topology is explicit endpoints only, never LAN discovery.

    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result, anyhow};
    use zenoh::Config;

    /// Default daemon socket: `$XDG_RUNTIME_DIR/tisvlay/zenoh.sock`, falling
    /// back to the system temp dir when no runtime dir is available.
    pub fn default_socket_path() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tisvlay")
            .join("zenoh.sock")
    }

    /// System-wide daemon socket installed by the packaged service
    /// (`RuntimeDirectory=tisvlay` → `/run/tisvlay/`).
    pub const SYSTEM_SOCKET_PATH: &str = "/run/tisvlay/zenoh.sock";

    /// Default daemon socket for the *connect* side. Prefers the
    /// per-user dev socket when it is **connectable**, then the
    /// packaged system daemon's socket, then falls back to the dev
    /// path (a resilient link dials it until a dev daemon appears).
    /// Connectable, not merely present: a dev stack that died
    /// uncleanly leaves its socket file behind, and zenoh's `.lock`
    /// takeover only rescues listeners — a client aimed at the
    /// corpse would dial it forever while the packaged daemon sits
    /// live next door. Listeners must use [`default_socket_path`] —
    /// a listener falling back to the system path would try to take
    /// over the packaged daemon's socket.
    pub fn default_connect_socket_path() -> PathBuf {
        resolve_connect_socket(&default_socket_path(), Path::new(SYSTEM_SOCKET_PATH))
    }

    fn resolve_connect_socket(dev: &Path, packaged: &Path) -> PathBuf {
        let dev_alive = std::os::unix::net::UnixStream::connect(dev).is_ok();
        if !dev_alive && packaged.exists() {
            return packaged.to_path_buf();
        }
        dev.to_path_buf()
    }

    /// Default session-service user socket: `$XDG_RUNTIME_DIR/tisvlay/
    /// session.sock`. The 0700 runtime dir is the privacy boundary.
    pub fn default_session_socket_path() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tisvlay")
            .join("session.sock")
    }

    /// Peer-mode config listening on `socket` — the daemon side.
    /// Timestamping is on: the advanced-publisher caches order and replay
    /// samples by timestamp.
    pub fn peer_config(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"peer\"")?;
        insert(&mut config, "listen/endpoints", &endpoint_list(&[socket])?)?;
        insert(&mut config, "timestamping/enabled", "true")?;
        Ok(config)
    }

    /// Router-mode hub listening on `socket` — the session service's
    /// user-socket side. A hub *peer* routes for client links but not
    /// between peer links, and the resilient consumers are connect-only
    /// peers — so a subtree where leaves must reach each other (the
    /// GUI querying the tray's settings service) needs a router hub.
    pub fn router_config(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"router\"")?;
        insert(&mut config, "listen/endpoints", &endpoint_list(&[socket])?)?;
        insert(&mut config, "timestamping/enabled", "true")?;
        Ok(config)
    }

    /// Client-mode config connecting to `socket` — one-shot callers
    /// (CLI, tests). Client `connect.timeout_ms` defaults to 0 (no
    /// retry), so an absent daemon fails fast at `zenoh::open`.
    ///
    /// One socket per session: a zenoh client maintains a single link
    /// (multiple connect endpoints are a failover list, not fan-out), so
    /// clients that span the system and user worlds open one session per
    /// socket. That is the gateway isolation made structural — nothing a
    /// client does can bridge the two key spaces.
    pub fn client_config(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"client\"")?;
        insert(&mut config, "connect/endpoints", &endpoint_list(&[socket])?)?;
        Ok(config)
    }

    /// Config for long-running consumers (GUI, tray, session service's
    /// system link): `zenoh::open` returns immediately and the link
    /// dials in the background forever, so no start or restart order is
    /// assumed — liveliness tokens are the availability signal, the role
    /// D-Bus's NameOwnerChanged played in the root stack.
    ///
    /// Shape: a connect-only *peer* (client mode requires a live link at
    /// open; peer mode backgrounds the dial). A session with exactly one
    /// endpoint and no listener cannot route between sockets, so the
    /// gateway isolation is untouched — processes spanning both worlds
    /// still hold one session per socket.
    pub fn client_config_resilient(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"peer\"")?;
        insert(&mut config, "listen/endpoints", "[]")?;
        insert(&mut config, "connect/endpoints", &endpoint_list(&[socket])?)?;
        insert(&mut config, "connect/exit_on_failure", "false")?;
        // Retry forever; don't gate open() on the first connect landing.
        insert(&mut config, "connect/timeout_ms", "-1")?;
        insert(
            &mut config,
            "open/return_conditions/connect_scouted",
            "false",
        )?;
        Ok(config)
    }

    fn base_config() -> Result<Config> {
        let mut config = Config::default();
        insert(&mut config, "scouting/multicast/enabled", "false")?;
        insert(&mut config, "scouting/gossip/enabled", "false")?;
        Ok(config)
    }

    fn insert(config: &mut Config, key: &str, value: &str) -> Result<()> {
        config
            .insert_json5(key, value)
            .map_err(|e| anyhow!("zenoh config {key} = {value}: {e}"))
    }

    /// JSON-encoded single-element endpoint list for `insert_json5`.
    fn endpoint_list(sockets: &[&Path]) -> Result<String> {
        let endpoints = sockets
            .iter()
            .map(|socket| {
                let path = socket
                    .to_str()
                    .context("socket path must be valid UTF-8 for a zenoh endpoint")?;
                Ok(format!("unixsock-stream/{path}"))
            })
            .collect::<Result<Vec<_>>>()?;
        serde_json::to_string(&endpoints).context("encode endpoint list")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn endpoint_list_embeds_absolute_path() {
            let list = endpoint_list(&[Path::new("/run/user/1000/tisvlay/zenoh.sock")]).unwrap();
            assert_eq!(
                list,
                r#"["unixsock-stream//run/user/1000/tisvlay/zenoh.sock"]"#
            );
        }

        /// Connect-side default resolution: a live dev listener wins; a
        /// stale dev socket file (dead listener) must fall through to
        /// the packaged path instead of aiming clients at a corpse; a
        /// missing packaged path leaves the dev default in place.
        #[test]
        fn stale_dev_socket_falls_through_to_the_packaged_path() {
            let dir = tisvlay_test::socket_tempdir();
            let dev = dir.path().join("zenoh.sock");
            let packaged = dir.path().join("packaged.sock");

            // Neither exists: dev default (resilient links dial it).
            assert_eq!(resolve_connect_socket(&dev, &packaged), dev);

            // Stale dev file, no packaged: still dev.
            let dead = std::os::unix::net::UnixListener::bind(&dev).unwrap();
            drop(dead);
            assert!(dev.exists());
            assert_eq!(resolve_connect_socket(&dev, &packaged), dev);

            // Stale dev file + packaged present: packaged wins.
            std::fs::File::create(&packaged).unwrap();
            assert_eq!(resolve_connect_socket(&dev, &packaged), packaged);

            // Live dev listener: dev wins again.
            let _live = std::os::unix::net::UnixListener::bind(dir.path().join("live.sock"));
            std::fs::remove_file(&dev).unwrap();
            let _dev_listener = std::os::unix::net::UnixListener::bind(&dev).unwrap();
            assert_eq!(resolve_connect_socket(&dev, &packaged), dev);
        }

        #[test]
        fn configs_build() {
            peer_config(Path::new("/tmp/tisvlay-test.sock")).unwrap();
            client_config(Path::new("/tmp/tisvlay-test.sock")).unwrap();
        }
    }
}

pub mod serve {
    //! The server half of the endpoint contract: a query's body, typed
    //! replies, and refusals. Reply failures (the querier gone) are logged,
    //! never fatal — the caller's own timeout reports them.

    use zenoh::bytes::Encoding;
    use zenoh::query::Query;

    use crate::endpoint::Rpc;
    use crate::error::WireError;

    /// A query's body bytes, empty when it carried none.
    pub fn payload(query: &Query) -> Vec<u8> {
        query
            .payload()
            .map(|payload| payload.to_bytes().into_owned())
            .unwrap_or_default()
    }

    /// Answer `query` with `R`'s response. Streaming rpcs call this once
    /// per finding.
    pub async fn reply<R: Rpc>(query: &Query, response: &R::Response) {
        let bytes = serde_json::to_vec(response)
            .expect("rpc responses are structs, which always encode as JSON");
        send(query, bytes).await;
    }

    /// Answer `query` with a response already encoded as JSON — the
    /// daemon's world replies, typed where the world built them.
    pub async fn reply_json(query: &Query, response: &serde_json::Value) {
        let bytes = serde_json::to_vec(response).expect("a JSON value always encodes as JSON");
        send(query, bytes).await;
    }

    /// Refuse `query` with a typed error.
    pub async fn reply_err(query: &Query, error: &WireError) {
        let bytes = serde_json::to_vec(error).expect("WireError always encodes as JSON");
        if let Err(e) = query
            .reply_err(bytes)
            .encoding(Encoding::APPLICATION_JSON)
            .await
        {
            tracing::warn!(key = %query.key_expr(), %e, "rpc refusal failed");
        }
    }

    async fn send(query: &Query, bytes: Vec<u8>) {
        let key = query.key_expr().clone().into_owned();
        if let Err(e) = query
            .reply(key, bytes)
            .encoding(Encoding::APPLICATION_JSON)
            .await
        {
            tracing::warn!(key = %query.key_expr(), %e, "rpc reply failed");
        }
    }
}
