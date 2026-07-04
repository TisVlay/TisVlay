//! Shared wire vocabulary for the zenoh-ecs spike.
//!
//! Everything both sides of the wire must agree on lives here: the key space,
//! the payload structs, and the session bootstrap (transport endpoint +
//! scouting discipline). Key-space reference: `docs/plans/spike-zenoh-ecs.md`
//! at the repo root.

pub mod payload {
    //! Wire payload structs — the load-bearing design decision of the spike:
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

    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Identity {
        pub name: String,
        pub vendor: String,
    }

    impl StatePayload for Identity {
        const PATH: &'static str = "meta/identity";
    }

    #[derive(Component, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct Meta {
        pub managed: bool,
    }

    impl StatePayload for Meta {
        const PATH: &'static str = "meta/config";
    }

    /// Every per-TV key tail — the DELETE sweep on TV removal.
    pub const TV_PATHS: [&str; 5] = [
        Power::PATH,
        Volume::PATH,
        Link::PATH,
        Identity::PATH,
        Meta::PATH,
    ];

    /// Pairing progress on `tisvlay/<site>/pairing/<uuid>/progress` — the
    /// redacted projection of the core pairing state: credentials never
    /// reach the wire, clients branch on `phase` and localize by `code`.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    pub struct PairingProgress {
        /// `awaiting_tv` | `awaiting_input` | `complete` | `failed`.
        pub phase: String,
        pub message: String,
        /// Namespaced failure reason (e.g. `pairing.rejected`), `failed` only.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub code: Option<String>,
    }

    impl From<&tisvlay_core::PairingState> for PairingProgress {
        fn from(state: &tisvlay_core::PairingState) -> Self {
            Self {
                phase: state.to_dbus_state().into(),
                message: state.message().into(),
                code: state.error().map(|e| e.code.as_str().into()),
            }
        }
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

    /// A per-TV key: `tisvlay/<site>/tv/<uuid>/<path>` where `path` is a
    /// [`StatePayload::PATH`](crate::payload::StatePayload::PATH) tail.
    pub fn tv_path(site: &str, tv: &Uuid, path: &str) -> String {
        format!("tisvlay/{site}/tv/{tv}/{path}")
    }

    /// Observability tap: `tisvlay/<site>/event/<name>`.
    pub fn event(site: &str, name: &str) -> String {
        format!("tisvlay/{site}/event/{name}")
    }

    /// Pairing progress stream: `tisvlay/<site>/pairing/<uuid>/progress`.
    pub fn pairing_progress(site: &str, tv: &Uuid) -> String {
        format!("tisvlay/{site}/pairing/{tv}/progress")
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
}

pub mod bootstrap {
    //! Session configuration for daemon (peer + UDS listener) and clients
    //! (connect-only). Multicast and gossip scouting are pinned off in both:
    //! the topology is explicit endpoints only, never LAN discovery.

    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result, anyhow};
    use zenoh::Config;

    /// Default daemon socket: `$XDG_RUNTIME_DIR/tisvlay/next.sock`, falling
    /// back to the system temp dir when no runtime dir is available.
    pub fn default_socket_path() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tisvlay")
            .join("next.sock")
    }

    /// Peer-mode config listening on `socket` — the daemon side.
    /// Timestamping is on: the advanced-publisher caches order and replay
    /// samples by timestamp.
    pub fn peer_config(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"peer\"")?;
        insert(&mut config, "listen/endpoints", &endpoint_list(socket)?)?;
        insert(&mut config, "timestamping/enabled", "true")?;
        Ok(config)
    }

    /// Client-mode config connecting to `socket` — CLI/GUI/test side.
    /// Client `connect.timeout_ms` defaults to 0 (no retry), so an absent
    /// daemon fails fast at `zenoh::open`.
    pub fn client_config(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"client\"")?;
        insert(&mut config, "connect/endpoints", &endpoint_list(socket)?)?;
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
    fn endpoint_list(socket: &Path) -> Result<String> {
        let path = socket
            .to_str()
            .context("socket path must be valid UTF-8 for a zenoh endpoint")?;
        serde_json::to_string(&[format!("unixsock-stream/{path}")]).context("encode endpoint list")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn endpoint_list_embeds_absolute_path() {
            let list = endpoint_list(Path::new("/run/user/1000/tisvlay/next.sock")).unwrap();
            assert_eq!(
                list,
                r#"["unixsock-stream//run/user/1000/tisvlay/next.sock"]"#
            );
        }

        #[test]
        fn configs_build() {
            peer_config(Path::new("/tmp/tvnext-test.sock")).unwrap();
            client_config(Path::new("/tmp/tvnext-test.sock")).unwrap();
        }
    }
}
