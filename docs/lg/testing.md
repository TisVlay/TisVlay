# LG Backend Testing

The `tisvlay-lg` crate has its own test infrastructure independent of the mock backend: a fake SSAP WebSocket server that speaks enough of the LG protocol to drive the real client through connection, registration, commands, and subscriptions.

For the workspace-wide testing framework, see [testing.md](../testing.md).

## FakeSsapServer

`crates/tisvlay-lg/src/test_server.rs` (~200 lines, `#[cfg(test)]`). Binds to `127.0.0.1:0`, accepts WebSocket connections, and dispatches SSAP messages by type.

```rust
use crate::test_server::{FakeSsapServer, RegistrationOutcome};

let server = FakeSsapServer::start().await;
let mut client = WebOsClient::connect_url(&server.ws_url(), None).await?;
// client is now registered and ready for commands
```

**FakeSsapHandle methods:**
- `set_registration_outcome(Accept | AcceptImmediate | Reject)` — control pairing flow
- `push_subscription_update(request_id, payload)` — inject state pushes
- `send_raw_text(text)` — inject malformed messages (fault injection)
- `drop_connection()` — close the WebSocket (reconnection testing)

## Test layers

| Layer | What | Location |
|-------|------|----------|
| Protocol fixtures | serde round-trips, fixture JSON parsing | `tests/protocol_fixtures.rs` + `tests/fixtures/*.json` |
| Config parsing | TOML with aliases, round-trip | `src/config.rs` (inline) |
| Client integration | registration, commands, subscriptions | `src/client.rs` (inline) |
| Backend traits | subscription merging, `LgConnection` surface | `src/connection.rs` (inline) |
| Fault injection | malformed JSON + connection drop (`src/client.rs`); `with_retry` (`src/connection.rs`) | inline |

## Connection seam

`Connection::connect_url_plain` is `pub(crate)` so tests can connect to `ws://127.0.0.1:{port}` instead of the hardcoded ports 3000/3001. `WebOsClient::connect_url()` wraps it as a `#[cfg(test)]` constructor.

TLS is skipped in tests — the `InsecureCertVerifier` accepts any cert, so there's no business logic to test. The wss→ws fallback is a trivial match.

## Known limitations

- **with_retry full reconnection**: `LgConnection::ensure_connected()` reconnects via `WebOsClient::connect(ip)` which hardcodes ports 3000/3001. Full end-to-end reconnect testing requires making the path URL-configurable.
- **Timeout paths**: `set_response_delay` not yet implemented on the server.
- **Partial handshake**: server closing during pairing phase 1 not yet tested.

These are diminishing-returns items — add them if a real bug surfaces in those paths.
