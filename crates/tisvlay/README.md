# tisvlay

[![CI Pipeline](https://github.com/TisVlay/TisVlay/actions/workflows/ci.yml/badge.svg)](https://github.com/TisVlay/TisVlay/actions/workflows/ci.yml)
[![Crates.io Version](https://img.shields.io/crates/v/tisvlay)](https://crates.io/crates/tisvlay)
[![Crates.io Dependencies](https://img.shields.io/deps-rs/tisvlay/latest)](https://crates.io/crates/tisvlay/dependencies)
![Crates.io MSRV](https://img.shields.io/crates/msrv/tisvlay)
![Crates.io Total Downloads](https://img.shields.io/crates/d/tisvlay)
[![Crates.io License](https://img.shields.io/crates/l/tisvlay)](https://choosealicense.com/licenses/apache-2.0)

Client library for TisVlay, the Linux daemon that controls smart TVs from the desktop they're plugged into. It lists and controls the TVs a TisVlay daemon manages (power, screen, volume, input, backlight), watches their state, and drives pairing, profiles and the per-user session and tray services.

This is the library. The `tisvlay` command ships in distro packages and the Nix flake (see the [repository](https://github.com/TisVlay/TisVlay)); `cargo install tisvlay` finds no binary to install.

## Example

```rust,no_run
use std::time::Duration;

use tisvlay::{DaemonClient, default_connect_socket_path};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> tisvlay::Result<()> {
    let daemon = DaemonClient::connect(&default_connect_socket_path(), "main").await?;
    // The client mirrors the daemon's published state in the background;
    // wait for the daemon to appear before reading it.
    while !daemon.is_available().await {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for tv in daemon.list_tvs().await? {
        println!("{} {} (connected: {})", tv.uuid, tv.name, tv.connected);
        daemon.screen_off(tv.uuid).await?;
    }
    Ok(())
}
```

## How it works

There are three clients: `DaemonClient` for the system daemon, and `SessionClient` and `TrayClient` for the per-user session service and the tray's settings service. A `StateWatcher` merges their change events into one stream.

- **Connecting.** A client's `connect` returns once its local session is open. It neither waits for nor requires the service: the link dials in the background for as long as the client lives, and availability follows the service's liveliness (`is_available`, and the `*Appeared` / `*Disappeared` events).
- **Mirror reads** send nothing. A client keeps a mirror of the state its service publishes, so a read such as `list_tvs` answers at once from the last published state. Each mirror empties while its service is down, with an event per row it drops, and fills again from the service's caches when it returns, so nothing it shows outlives the service.
- **Rpcs** are round trips: commands, edits, pairing. Any rpc can fail with `Error::Unavailable`, `Error::Timeout`, `Error::VersionMismatch`, `Error::Transport` or `Error::Protocol`. A refusal is `Error::Rpc`, and each method's docs name the `ErrorCode`s it can be refused with.

## Requirements

- **A tokio multi-thread runtime.** The clients and the watcher spawn tasks on the ambient runtime, which must be the multi-thread scheduler (zenoh, which carries the protocol, rejects the current-thread one). Construct them from inside that runtime.
- **Linux or another Unix.** The services listen on Unix sockets.
- **Access to the daemon's socket.** The packaged daemon grants it to the `tisvlay` group and to the active seat's user.

## Compatibility

A client talks only to services of its own release line, the same `major.minor`: SDK 0.4.x works with services 0.4.x, and patch releases interoperate (a pre-release such as `0.5.0-rc.1` works only with exactly itself). Each client checks its service's release in each lifetime of the service, starting when the service appears and retrying until it gets an answer. The answer is a mirror read, `service_info` (the service's release and state path), announced by `StateEvent::ReleaseChecked`. On a mismatch, its rpcs fail with `Error::VersionMismatch`, its mirror reads come back empty (`list_tvs` returns the error), and `StateEvent::VersionMismatch` fires, until the service restarts on a matching release. Upgrade the SDK with the daemon.

## Public dependencies

These crates' types appear in this crate's API, so a major-version bump of one is a breaking change here:

- `tisvlay-core`: `TvInfo`'s capabilities, hardware id and power state, pairing schema fields, and the profile configs.
- `tisvlay-protocol`: the re-exported payload and rpc types and `ErrorCode`. It releases in lockstep with this crate.
- `uuid`: TV and profile ids.
- `serde`: the re-exported protocol payload and rpc types implement `Serialize` and `Deserialize`.
- `serde_json`: `update_tv_config` takes a `serde_json::Map`.
- `futures-core`: the pairing, power-cycle and discovery progress streams are `impl Stream`.

## License

Apache-2.0.
