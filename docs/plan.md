# Plan: LG Backend Coverage (Fake SSAP Server)

> For the current testing framework docs, see [testing.md](testing.md).

## Status: complete

All three layers delivered. tisvlay-lg went from ~1% to ~60% line
coverage; workspace from 41% to 54%.

| Layer | Tests | Coverage impact |
|-------|-------|-----------------|
| 1 — Protocol unit tests | 24 | protocol.rs 0%→99%, config.rs 0%→100%, discovery.rs 33%→45% |
| 2 — Fake SSAP server | 22 | client.rs 0%→54%, connection.rs 0%→44% |
| 2.9 — Backend trait tests | 17 | backend.rs 2%→73% |
| 3 — Fault injection | 3 | connection.rs 30%→44%, with_retry error path covered |

**Total: 66 new tests** in tisvlay-lg, ratchet bumped 40%→51%.

### What was built

- `crates/tisvlay-lg/src/test_server.rs` — fake SSAP WebSocket server
  (~200 lines) with `FakeSsapHandle` for test control: registration
  outcomes, subscription pushes, raw text injection, connection drop
- `crates/tisvlay-lg/tests/protocol_fixtures.rs` — fixture-based
  parsing tests with 10 JSON files
- `crates/tisvlay-lg/src/config.rs` — 7 unit tests inline
- `crates/tisvlay-lg/src/backend.rs` — 20 unit + integration tests
  inline (subscription merging, config builder, trait surface, faults)
- `crates/tisvlay-lg/src/client.rs` — 16 integration tests inline

### Known limitations

- **with_retry full reconnection**: `LgConnection::ensure_connected()`
  uses `WebOsClient::connect(ip)` which hardcodes ports 3000/3001.
  The fake server listens on a random port. Full end-to-end reconnect
  testing requires making the reconnect path URL-configurable.
- **Timeout paths**: `set_response_delay` on the server not yet
  implemented. Would test `tokio::time::timeout` in `Connection::request`.
- **Partial handshake**: server closing during pairing phase 1 not
  yet tested.

These are diminishing-returns items that can be added if a real bug
surfaces in those paths.

## Out of scope

- **TLS testing** — `InsecureCertVerifier` is a passthrough, no business logic
- **SSDP multicast testing** — needs real network interfaces
- **Real TV integration tests** — probe scripts serve this role manually
- **tisvlay-tray coverage** — ksni rendering, needs GUI test framework
- **tisvlay-session/main.rs** — entry point wiring, not worth testing
