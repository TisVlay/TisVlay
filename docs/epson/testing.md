# Epson backend testing

The `tisvlay-epson` crate is fully testable with no projector: a fake ESC/VP21 TCP server that speaks the `ESC/VP.net` handshake and answers scripted replies drives the real client through connect, handshake, command exchange, warmup/cooldown progression, and socket-drop reconnection. Mirrors the LG backend's [`FakeSsapServer`](../lg/testing.md) shape.

For the workspace-wide testing framework, see [testing.md](../testing.md). For the ESC/VP21 wire protocol the fake server emulates, see [protocol.md](protocol.md).

## FakeEpsonServer

[`crates/tisvlay-epson/src/test_server.rs`](../../crates/tisvlay-epson/src/test_server.rs) (`#[cfg(test)] pub(crate)`). `FakeEpsonServer::start()` binds `127.0.0.1:0`, returns a `FakeEpsonHandle`, and spawns a `tokio` accept loop that handles one connection at a time, **re-accepting after a drop** so reconnection tests work. Each connection performs the 16-byte handshake (echoing `ESC/VP.net` with the `0x20` status byte), then reads `\r`-terminated command bodies and answers from shared, scriptable state — queries as `NAME=VALUE\r:`, sets as a bare `:` ack that also applies the set's effect to the state.

```rust
use crate::test_server::FakeEpsonServer;

let server = FakeEpsonServer::start().await;
let mut client = EpsonClient::connect(server.host(), server.port()).await?;
// client is handshaked and ready for commands
```

### Scripting hooks

`FakeEpsonHandle` exposes the knobs the tests drive (see [`test_server.rs`](../../crates/tisvlay-epson/src/test_server.rs) for the live surface):

- `host()` / `port()` / `addr()` — the loopback endpoint the client connects to (the connection seam).
- `set_power_code(code)` — set the steady-state `PWR?` code (`"01"`, `"04"`, …).
- `set_mute(bool)` — set the A/V-mute state directly.
- `set_source(code)` — set the `SOURCE?` hex code directly.
- `script_warmup(count, code)` — after a `PWR ON`, answer the next `count` `PWR?` reads with `code` (the warming code, or a deliberately indistinct busy code to exercise the intent fallback), then snap to `"01"`. Marks the projector on so the warming reads precede the on reading.
- `script_cooldown(count, code)` — after a `PWR OFF`, answer the next `count` `PWR?` reads with `code`, then report standby (`"04"`).
- `drop_connection()` — drop the active TCP connection (as the projector does on standby) so the next client command must reconnect.

The warmup/cooldown progression lives on the shared server state and advances per `PWR?` read, so a test can poll the same way the daemon's poll loop would and watch the projector walk `WarmingUp → Active` (or `CoolingDown → Standby`).

## Connection seam

The client connects to whatever `host:port` it is handed, so tests point it at the fake server's ephemeral loopback port directly — there is no hardcoded production port to override. [`EpsonVendorConfig::port`](../../crates/tisvlay-epson/src/config.rs) defaults to 3629 but is overridable for exactly this reason (and is deliberately *not* in the config schema — no user-facing port selector until a second transport lands). There is no TLS in the ESC/VP21 transport, so nothing is stubbed away on the crypto side.

## Test layers

The tests are layered from the pure codec up to the connector, mirroring the [vendor-backend guide's](../backends/implementing-a-vendor.md) recommended progression. All live as inline `#[cfg(test)] mod tests` in their respective source files (there is no separate `tests/` directory).

| Layer | What | Location |
|-------|------|----------|
| Protocol codec | handshake bytes, command framing, reply parsing, power-code / mute / source value tables, intent resolution | `src/escvp21.rs` (inline, table-driven, I/O-free) |
| Config parsing | TOML round-trip, port default + override, required-`ip` rejection, schema field visibility | `src/config.rs` (inline) |
| Client / connection integration | power-on through warmup → `Active`, screen via A/V mute, input switch + read-back, backlight via `LUMLEVEL`, `get_state` composition, reconnect-after-drop, intent-disambiguated warmup vs cooldown | `src/connection.rs` (inline, against `FakeEpsonServer`) |
| Connector | live `connect`, fail-fast on a dead address, the no-auth `start_pairing` connectivity check (success + unreachable), wake gating on `mac` | `src/connector.rs` (inline, against `FakeEpsonServer`) |
| Backend metadata | vendor id/name, capability shape, `verify_retry_config` slower than the LG default | `src/backend.rs` (inline) |

## Known limitations

`FakeEpsonServer` answers the four commands the backend uses and returns `ERR\r:` for anything else; it is not a full ESC/VP21 emulator. It does not model command-timeout paths (no response-delay hook) or a mid-handshake drop. These are diminishing-returns items — add them if a real bug surfaces in those paths, the same posture as the LG fake server. The intent-disambiguation correctness against *real* firmware is a hardware-validation item, not something the fake server can settle — see [firmware-quirks.md](firmware-quirks.md).
