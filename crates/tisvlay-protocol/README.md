# tisvlay-protocol

The protocol TisVlay's services and clients speak: zenoh over Unix sockets, with a key space for published state, typed rpcs for commands, and liveliness tokens for presence. The [`tisvlay`](https://crates.io/crates/tisvlay) crate builds a client SDK on it. Use this crate directly to write a client of another shape, or a stand-in service for tests.

## What's in it

- `keys`: the key space. Builders for every key under `tisvlay/<site>/`, and parsers for the keys a wildcard subscription delivers.
- `payload`: the published state, one serde struct per key, and the progress payloads.
- `rpc`: request and response bodies.
- `endpoint`: the rpc families. Each table declares its rpcs once and generates both the client calls and the server's typed `Incoming` dispatch.
- `caller`: the client side. A typed call that waits for a service to match before sending, with timeouts.
- `serve`: the server side. The typed `Responder` each decoded rpc carries, and `ParkedReply` for answering later.
- `error`: `WireError` and its `ErrorCode`, the refusals a service returns.
- `bootstrap`: zenoh session configs for connecting and listening (network discovery is always off) and the default socket paths.

## Example

Ask the daemon its release:

```rust,no_run
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::manager;
use tisvlay_protocol::{VERSION, bootstrap, compatible};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let socket = bootstrap::default_connect_socket_path();
    let session = zenoh::open(bootstrap::client_config(&socket)?).await?;
    let caller = Caller::new(session, "main");
    let info = caller.call::<manager::Info>(&(), &()).await?;
    println!("daemon {} (compatible: {})", info.version, compatible(VERSION, &info.version));
    Ok(())
}
```

## Compatibility

Services and clients built from the same `major.minor` speak the same protocol; patch releases interoperate. A pre-release (`0.5.0-rc.1`) talks only to exactly itself, since it may still change before its final release. `VERSION` is this crate's release, which every service reports in its `info` reply, and `compatible()` holds the rule. `ServiceInfo` and the `info` keys are frozen, so a client of any release can read a service's release. There are no compatibility shims beyond that: a different release line is refused, not translated.

## Requirements

A tokio multi-thread runtime (zenoh rejects the current-thread scheduler), and Linux or another Unix for the Unix-socket transport.

## Public dependencies

These crates' types appear in this crate's API, so a major-version bump of one is a breaking change here:

- `zenoh`: `Caller::new` takes a `zenoh::Session`, `Incoming::receive` a `zenoh::query::Query`, the config builders return `zenoh::Config`, and the errors carry `zenoh::Error`.
- `tisvlay-core`: payload fields (`PowerState`, capabilities), the failure codes mapped from core's, and `DiscoveredTv` on the discovery stream.
- `serde` and `serde_json`: every payload and rpc body is a serde type, and `ParkedReply` answers with a `serde_json::Value`.
- `uuid`: TV, profile and session ids in keys and bodies.

## License

Apache-2.0.
