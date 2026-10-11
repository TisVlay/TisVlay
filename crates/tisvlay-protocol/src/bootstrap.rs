//! Session configuration for the services (a listener on a Unix socket)
//! and their clients (connect-only). Multicast and gossip scouting are
//! pinned off in every config: the topology is explicit endpoints only,
//! never LAN discovery.

use std::fmt;
use std::path::{Path, PathBuf};

use zenoh::Config;

/// A session config that couldn't be built.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfigError {
    /// The socket path isn't valid UTF-8, which a zenoh endpoint needs.
    NonUtf8Path(PathBuf),
    /// zenoh refused a config value.
    Rejected {
        /// The config key.
        key: String,
        /// The value it refused.
        value: String,
        /// zenoh's reason.
        source: zenoh::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8Path(path) => write!(
                f,
                "socket path {} isn't valid UTF-8, which a zenoh endpoint needs",
                path.display()
            ),
            Self::Rejected { key, value, .. } => write!(f, "zenoh config {key} = {value}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NonUtf8Path(_) => None,
            Self::Rejected { source, .. } => Some(source.as_ref()),
        }
    }
}

type Result<T> = std::result::Result<T, ConfigError>;

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
        .map_err(|source| ConfigError::Rejected {
            key: key.to_owned(),
            value: value.to_owned(),
            source,
        })
}

/// JSON-encoded single-element endpoint list for `insert_json5`.
fn endpoint_list(sockets: &[&Path]) -> Result<String> {
    let endpoints = sockets
        .iter()
        .map(|socket| {
            let path = socket
                .to_str()
                .ok_or_else(|| ConfigError::NonUtf8Path(socket.to_path_buf()))?;
            Ok(format!("unixsock-stream/{path}"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(serde_json::to_string(&endpoints)
        .expect("a list of strings serializes, because JSON can encode any string"))
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
        let socket = Path::new("/tmp/tisvlay-test.sock");
        peer_config(socket).unwrap();
        router_config(socket).unwrap();
        client_config(socket).unwrap();
        client_config_resilient(socket).unwrap();
    }

    #[test]
    fn a_non_utf8_socket_path_is_a_typed_error() {
        use std::os::unix::ffi::OsStrExt;
        let socket = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff.sock"));
        assert!(matches!(
            client_config(socket),
            Err(ConfigError::NonUtf8Path(path)) if path == socket
        ));
    }
}
