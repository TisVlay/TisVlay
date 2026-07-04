//! Shared wire vocabulary for the zenoh-ecs spike.
//!
//! Everything both sides of the wire must agree on lives here: the key space
//! and the session bootstrap (transport endpoint + scouting discipline).
//! Key-space reference: `docs/plans/spike-zenoh-ecs.md` at the repo root.

pub mod keys {
    //! Key-expression builders. One function per key family so the literal
    //! shape exists in exactly one place.

    /// Liveliness token declared by a running daemon for `site`.
    pub fn daemon_alive(site: &str) -> String {
        format!("tisvlay/{site}/alive/daemon")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn daemon_alive_key_shape() {
            assert_eq!(daemon_alive("main"), "tisvlay/main/alive/daemon");
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
    pub fn peer_config(socket: &Path) -> Result<Config> {
        let mut config = base_config()?;
        insert(&mut config, "mode", "\"peer\"")?;
        insert(&mut config, "listen/endpoints", &endpoint_list(socket)?)?;
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
