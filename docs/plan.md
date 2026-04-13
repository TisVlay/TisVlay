# Plan: LG Backend Coverage (Fake SSAP Server)

> For the current testing framework docs, see [testing.md](testing.md).
> This plan tracks the work to bring `tisvlay-lg` from ~1% to 60-70%
> line coverage via a layered approach: unit tests on protocol types,
> a fake SSAP server for integration tests, and fault injection for
> error paths.

## Goal

Test the LG WebOS backend (`tisvlay-lg`) without a real TV on the
network. Currently at ~1% coverage (only `discovery.rs` parsing has
any hits). The backend is 1,020 lines of untested protocol handling,
connection management, retry logic, and subscription routing.

## Approach: three layers, each building on the last

### Layer 1 — Protocol unit tests (no server needed)

Pure serde round-trip and fixture-based parsing tests for `protocol.rs`,
`config.rs`, and the SSDP response parser in `discovery.rs`.

**What it tests:**
- `Request` / `Response` serialization round-trips
- Parsing real SSAP JSON captured from TV interactions (registration
  handshake, subscription responses, command responses, error payloads)
- `LgVendorConfig` TOML parsing (valid, missing fields, defaults)
- SSDP response header parsing (UUID extraction, IP extraction, model
  name)

**Fixture strategy:**
- `crates/tisvlay-lg/tests/fixtures/*.json` — captured or reconstructed
  SSAP messages, one per file
- `include_str!` to embed at compile time
- `rstest` parameterized tests across fixture files
- `insta` snapshots for serialized output

**Estimated coverage impact:** tisvlay-lg ~1% → ~20%

### Layer 2 — Fake SSAP server (localhost WebSocket)

A lightweight `tokio-tungstenite` server that speaks enough SSAP to
drive the LG client through its real code paths: connection, registration,
commands, subscriptions, and `with_retry` reconnection.

**What it tests:**
- `Connection` — WebSocket connect, request/response ID correlation,
  subscription routing, reader/writer split, Pong handling
- `WebOsClient` — registration handshake (two-phase: `Response` then
  `Registered`), command dispatch, subscription setup,
  `subscribe_state_updates()` state merging
- `LgConnector` — `connect()` (ws:// path), `start_pairing()` two-phase
  flow, `build_config_from_pairing()` credential storage
- `LgConnection` — full `TvConnection` trait surface (`get_state`,
  `set_volume`, `screen_off`, `switch_input`, `set_backlight`,
  `subscribe_state`, `power_off`)
- `with_retry` — detect stale socket, reconnect, replay command

**Fake server design:**

Build directly on `tokio-tungstenite::accept_async` (~150-200 lines).
No external mock-WebSocket crate — SSAP's stateful, ID-correlated,
multi-phase protocol doesn't fit matcher-based frameworks (ws-mock,
wiremocket). Research confirmed this (see below).

```
FakeSsapServer
├── bind to 127.0.0.1:0 (OS-assigned port)
├── accept loop (supports reconnection testing)
├── message handler:
│   ├── register → two-phase Response + Registered
│   ├── subscribe → echo response + push updates via handle
│   └── command → echo success with request ID
└── FakeSsapHandle (test control surface)
    ├── push_subscription_update(uri, payload)
    ├── set_registration_outcome(accept | reject | ignore)
    ├── drop_connection()  (for with_retry testing)
    └── port() → u16
```

**Location:** `crates/tisvlay-lg/src/test_server.rs`, gated behind
`#[cfg(test)]`. The fake needs intimate knowledge of SSAP message
formats (request ID extraction, response construction) which are
`pub(crate)` types in the same crate.

**Connection seam:** The LG `Connection` already has
`connect_ws(host, timeout)` which builds a `ws://host:3000` URL.
For tests, we need to connect to `ws://127.0.0.1:{random_port}`
instead. Two options:

- **(a)** Add `Connection::connect_url_plain` as `pub(crate)` (it's
  currently private) so tests can pass `ws://127.0.0.1:{port}`
  directly.
- **(b)** Make `from_stream(WsStream)` test-accessible so tests can
  inject a pre-connected stream.

Option (a) is cleaner — no `#[cfg(test)]` needed, just a visibility
change on an existing method.

**TLS:** Skip. Test with ws:// only. The `InsecureCertVerifier`
accepts any cert, so TLS verification is a passthrough with no
business logic. The wss→ws fallback path is a ~5-line match that
can be covered by a targeted unit test if needed. If we ever need
TLS in tests, `rcgen` generates self-signed certs in one call.

**Estimated coverage impact:** tisvlay-lg ~20% → ~60-70%

### Layer 3 — Fault injection (on top of Layer 2)

Extend the fake server to simulate network failures and malformed
responses. Tests exercise error paths that real TVs produce in
practice (firmware updates, Wi-Fi blips, TV standby closing socket).

**What it tests:**
- Drop WebSocket mid-session → reconnection logic in `with_retry`
  and `LgConnector::connect()` retry loop
- Send malformed JSON → error handling in `Connection::reader_loop`
- Delay response past timeout → `tokio::time::timeout` paths
- Close during pairing phase 1 → partial-handshake recovery
- Send unknown message types → subscription channel handling
- Connection refused → `connect()` error path

**Server extensions:**
- `handle.drop_connection()` already in Layer 2 for `with_retry`
- `handle.send_raw(bytes)` — inject arbitrary WebSocket messages
- `handle.set_response_delay(duration)` — simulate slow TV
- `handle.reject_next_connection()` — refuse TCP accept

**Estimated coverage impact:** tisvlay-lg ~60-70% → ~80%+

## Research findings

### Why not ws-mock or wiremocket?

Both are matcher/assertion frameworks for simple WebSocket interactions.
SSAP requires:
- Request-ID-based correlation (server must parse `id` from request
  and echo it in response)
- Multi-phase registration (Response then Registered for the same ID)
- Server-initiated subscription pushes keyed by request ID
- Stateful protocol (registration must complete before commands work)

Neither crate supports "read JSON, extract `id`, construct custom
response with that ID." A custom ~150-line server using
`tokio-tungstenite::accept_async` is simpler and maps directly to
SSAP semantics.

### No existing SSAP test server exists

Searched Python (bscpylgtv), Node.js (lgtv2), Go (go-webos), and
Rust (lgtv_manager) libraries. None have a mock/fake SSAP server.
All test against real hardware or skip protocol-level tests entirely.

### Fixture validation

Real SSAP JSON can be captured from the probe scripts at
`scripts/probe_subscriptions.py` and `scripts/probe_volume_and_backlight.py`
against the user's LG C3/G4 TVs. Alternatively, fixtures can be
reconstructed from `protocol.rs` types and validated against the TV
if anything looks off.

## Implementation order

| Step | Layer | What | Files |
|------|-------|------|-------|
| 1 | 1 | `protocol.rs` serde round-trip + fixture parsing | `tests/protocol_fixtures.rs`, `tests/fixtures/*.json` |
| 2 | 1 | `config.rs` TOML parsing | `tests/config_parse.rs` or unit tests in `config.rs` |
| 3 | 1 | `discovery.rs` SSDP response parsing | unit tests in `discovery.rs` |
| 4 | 2 | `FakeSsapServer` + `FakeSsapHandle` skeleton | `src/test_server.rs` |
| 5 | 2 | Registration handshake test (connect + register) | `tests/ssap_integration.rs` |
| 6 | 2 | Command round-trip tests (volume, input, screen, backlight) | same file |
| 7 | 2 | Subscription push tests (state updates arrive via channel) | same file |
| 8 | 2 | `with_retry` reconnection test (server drops, client reconnects) | same file |
| 9 | 2 | `LgConnector` + `LgConnection` trait impl tests | `tests/lg_connection.rs` |
| 10 | 3 | Malformed JSON / unknown message handling | `tests/ssap_faults.rs` |
| 11 | 3 | Timeout paths (delayed response, hung server) | same file |
| 12 | 3 | Partial handshake (close during pairing phase 1) | same file |

## Out of scope

- **TLS testing** — `InsecureCertVerifier` is a passthrough, no
  business logic. Use `rcgen` if ever needed.
- **SSDP multicast testing** — needs real network interfaces.
  Parsing is covered by Layer 1 unit tests.
- **Real TV integration tests** — useful for validation but not CI.
  The probe scripts serve this role manually.
- **tisvlay-tray coverage** — ksni rendering, needs GUI test framework.
- **tisvlay-session/main.rs** — entry point wiring, not worth testing.
