# Epson ESC/VP21-over-TCP control reference

The wire protocol the [`tisvlay-epson`](../../crates/tisvlay-epson) backend speaks to an Epson projector: the `ESC/VP.net` handshake, command/reply framing, the command vocabulary, and the power-code state machine. Seeded from research against the target hardware (Epson Pro Cinema LS12000) plus the community ESC/VP21 integrations — cells marked **TBD** await validation on a real LS12000 and are tracked in [firmware-quirks.md](firmware-quirks.md). For the backend's test infrastructure (`FakeEpsonServer`) see [testing.md](testing.md). The pure codec that implements everything below lives in [`crates/tisvlay-epson/src/escvp21.rs`](../../crates/tisvlay-epson/src/escvp21.rs); the TCP plumbing is in [`transport.rs`](../../crates/tisvlay-epson/src/transport.rs) and the command sequencing in [`client.rs`](../../crates/tisvlay-epson/src/client.rs).

## Transport

ESC/VP21 travels over a raw TCP socket to `<projector-ip>:3629`. There is no TLS and no authentication — the socket is the trust boundary, so the daemon only opens it on a LAN it already trusts. The projector closes its TCP socket when it enters standby, so the client treats a dead socket as expected and re-handshakes on the next command (see [`EpsonClient::command`](../../crates/tisvlay-epson/src/client.rs)).

### Handshake

On connect, the client sends a fixed 16-byte greeting and validates the projector's 16-byte reply:

| Field | Bytes | Value |
|-------|-------|-------|
| Greeting (client → projector) | `[0..10]` | ASCII `ESC/VP.net` |
| | `[10..16]` | `10 03 00 00 00 00` (hex) |
| Reply (projector → client) | `[0..10]` | ASCII `ESC/VP.net` (echoed) |
| | `[14]` | `0x20` (ASCII space) on success |

The reply is good iff bytes `[0..10] == "ESC/VP.net"` **and** byte `[14] == 0x20`. Anything shorter, or with a mismatched prefix or status byte, is a failed handshake. The greeting constant and the validator are [`escvp21::HELLO`](../../crates/tisvlay-epson/src/escvp21.rs) and `escvp21::handshake_ok`.

### Command / reply framing

After the handshake, control is line-oriented:

- A **command** is the command name optionally followed by a space and an argument, terminated with a carriage return: `NAME[ ARG]\r` (e.g. `PWR ON\r`, `SOURCE 30\r`).
- A **query** is the command name with a `?` suffix: `NAME?\r` (e.g. `PWR?\r`).
- A successful **query reply** is `NAME=VALUE` terminated by `\r:` (e.g. `PWR=01\r:`). The value is the part after `=`.
- A successful **set** returns a bare `:` acknowledgement (no `NAME=`).
- The projector returns the literal `ERR` for an unrecognized or rejected command.

The projector pads a fixed-size read, so a reply frame can carry trailing NUL bytes after the `\r:` terminator; the codec strips trailing `\r`, `:`, and `\0` before classifying. The framing helpers and the `Reply` enum (`Value` / `Ack` / `Error`) live in [`escvp21.rs`](../../crates/tisvlay-epson/src/escvp21.rs).

## Command vocabulary

The four commands the backend uses. The codec exposes these names under [`escvp21::cmd`](../../crates/tisvlay-epson/src/escvp21.rs).

| Concern | Query | Set | Reply / arg | Maps to |
|---------|-------|-----|-------------|---------|
| Power | `PWR?` | `PWR ON` / `PWR OFF` | status code (below) | `PowerState`, `power_off`/`power_on` |
| A/V mute (screen blank) | `MUTE?` | `MUTE ON` / `MUTE OFF` | `ON` / `OFF` | `screen_off` / `screen_on` |
| Input source | `SOURCE?` | `SOURCE <hex>` | hex source code (below) | `switch_input` / `get_current_input` |
| Laser brightness | `LUMLEVEL?` | `LUMLEVEL <n>` | integer (range TBD) | `set_backlight` / `get_backlight` |

ESC/VP21 has a wider vocabulary than this (`LUMINANCE` laser-power mode, `CMODE` color mode, lamp-hours, and more); the backend implements only the four that map onto a `TvConnection` capability for v1. The rest are candidates for a future diagnostics surface — see [the lamp-hours diagnostic idea](../ideas/projector-lamp-hours-diagnostic.md).

### Power-code state machine

`PWR?` returns a two-character status code. The codec ([`escvp21::parse_power_code`](../../crates/tisvlay-epson/src/escvp21.rs) → `power_state_from`) classifies it, then resolves it into a [`PowerState`](../../crates/tisvlay-core/src/state.rs):

| `PWR?` code | Classification | `PowerState` | Verified on hardware? |
|:-----------:|----------------|--------------|:---:|
| `01` | on | `Active` (or `ScreenOff` if A/V-muted — see below) | TBD |
| `02` | warming up | `WarmingUp` | **TBD — see note** |
| `03` | cooling down | `CoolingDown` | **TBD — see note** |
| `00` / `04` / `05` | standby / off | `Standby` | TBD |
| anything else | unknown busy | resolved by intent (below) | TBD |

```
Standby (00/04/05)  --PWR ON-->  WarmingUp (~30–40 s)  -->  Active (01)
Active (01)         --PWR OFF--> CoolingDown (~10 s)   -->  Standby
```

**Whether `02` and `03` are actually directionally distinct on the LS12000 is UNVERIFIED.** The Python reference library that informed this design only recognizes `02` as a *generic busy* code, so it is plausible the firmware reports the same busy code for both warmup and cooldown. The backend handles both possibilities:

- If the firmware *does* return a directionally-distinct code (`02` warming, `03` cooling), that classification wins outright.
- If the firmware returns one indistinct busy code for both directions (classified `Unknown`), the client disambiguates it against the **last power command it sent** — a tracked `PowerIntent` of `PoweringOn` resolves to `WarmingUp`, `PoweringOff` to `CoolingDown`. `EpsonClient` records the intent on every `PWR ON` / `PWR OFF`.

This intent-disambiguation is wiring that the fake-server tests exercise; its correctness against real firmware is a hardware-validation item, not a tested fact.

### Screen = A/V mute

ESC/VP21 has no separate "screen off" state — a projector that is on with the image blanked still reports `PWR?=01`. The "screen off" concept maps onto the projector's **A/V mute**: `MUTE ON` blanks the projected image (the projector analog of OLED screen-off — it idles the panel and saves laser hours), `MUTE OFF` projects it. So the backend derives the compound power state one layer up, in [`EpsonClient::get_power_state`](../../crates/tisvlay-epson/src/client.rs):

- `PWR?=01` **and** `MUTE?=OFF` → `PowerState::Active`
- `PWR?=01` **and** `MUTE?=ON` → `PowerState::ScreenOff`

This is the same compound derivation the LG backend already does for its on-but-screen-off substate, so the daemon's reliable `screen_on` / `screen_off` dispatch (which is agnostic about how `ScreenOff` is computed) needs no projector-specific handling.

### Input-source codes

`SOURCE?` / `SOURCE <hex>` use hex source codes; the backend normalizes them to opaque input IDs (`source_code_to_input` / `input_to_source_code` in [`escvp21.rs`](../../crates/tisvlay-epson/src/escvp21.rs)):

| Hex code | Normalized input ID | Verified? |
|:--------:|---------------------|:---:|
| `30` | `HDMI_1` | TBD |
| `A0` | `HDMI_2` | TBD |
| `10` | `PC` | TBD |
| `40` / `41` | `VIDEO` (round-trips to `40`) | TBD |
| `52` | `USB` | TBD |
| `53` | `LAN` | TBD |

ESC/VP21 has no per-input connection probe, so the advertised input list reports `connected: false` for every entry — it carries capability, not live link status.

### Backlight = LUMLEVEL

`LUMLEVEL` is the projector's continuous laser-brightness control — dimming it directly serves the "preserve laser hours" use case, which is why it maps onto the `BACKLIGHT` capability. `LUMLEVEL 0` is the minimum; `LUMLEVEL?` reads the current value. **The real range and units are UNVERIFIED**; v1 treats the value as a 0–100 percentage, clamping a larger reading down to 100. Confirming the range is a hardware-validation item.

## Standby Mode = Communication On

For the projector to accept network control while in standby — and for the wake path to work — the projector's **Standby Mode** setting must be **Communication On** (an on-device setting, not something the protocol can toggle). With it off, the network interface powers down in standby and the projector is unreachable until woken by its own remote or WoL. The trade-off is standby power draw: roughly **0.4 W** with Communication Off versus **~2 W** with Communication On (figures from the LS12000 documentation; TBD on the bench).

## No push / event channel

ESC/VP21 has **no push, subscription, event, or notification channel** — confirmed authoritatively from Epson's Web API specification v1.1.1, whose only "events" are digital-signage playback schedules, not state-change notifications. There is no WebSocket, SSE, long-poll, or webhook anywhere in the Epson control surface, and Epson's own first-party app polls. The backend therefore leaves `subscribe_state()` at its `NotSupported` default and the daemon falls back to periodic polling — the correct and only path. (An adaptive poll cadence during warmup/cooldown is a [deferred idea](../ideas/poll-rate-during-power-transition.md).)

## Transport landscape (dated)

The control vocabulary above is ESC/VP21 regardless of transport; what differs is the carrier. As of 2026, the relevant carriers for the LS12000:

| Transport | Port | Status | Capability reach | Auth |
|-----------|:----:|--------|------------------|------|
| **TCP 3629 (ESC/VP21)** | 3629 | **Used in v1.** Firmware-robust — works on every firmware version, updated or not. | Richest path: the only one that also reaches `LUMLEVEL` (backlight) and `MUTE` (screen-blank), not just power. | None |
| Web Control / Web API | 80/443 | Future transport. Arrived on the LS12000 in firmware **v2.00**; present and maintained through **v3.04 (2026-04-22)**. Version-gated. | No confirmed capability edge over TCP 3629. | `EPSONREMOTE` (Basic Control) / `EPSONWEB` realms; no password by default. |
| PJLink | 4352 | Future transport. Works on the LS12000. | Power + input only ("turning it on/off works, but that's it"). | PJLink password; no password by default. The PJLink and Web Control passwords are *separate* realms. |

TCP 3629 is the v1 choice because it is both the most firmware-robust (no v2.00+ dependency) and the richest working path. The Web API and PJLink are documented here as clean future additions behind the command/transport boundary in [`transport.rs`](../../crates/tisvlay-epson/src/transport.rs) — that module keeps the high-level command sequencing free of socket details so promoting it to a multi-transport seam is a localized change. A future PJLink transport is notable for two reasons it could *sharpen* the model: its power query reports directionally-distinct warmup/cooldown natively (replacing the intent inference), and it exposes lamp/laser-hours. The [`pjlink`](https://lib.rs/crates/pjlink) Rust crate is the candidate dependency if that transport is built.

## Discovery

Discovery is deferred entirely to a hardware-validation follow-up; the backend does not declare `Capabilities::DISCOVERY` in v1. When revisited, the LS12000's *documented* discovery mechanisms are **PJLink**, **AMX Device Discovery**, and **Control4 SDDP** — **not** mDNS/SSDP — so the v4-only `tisvlay-ssdp` transport does not apply. See [docs/discovery.md](../discovery.md) § Trait boundary.

## References & prior art

ESC/VP21-over-TCP is well-trodden; these implementations informed this backend and are worth consulting when extending it:

- [openHAB Epson Projector binding](https://www.openhab.org/addons/bindings/epsonprojector/) — the most mature FOSS ESC/VP21 implementation (Java), covering both TCP and serial and auto-discovering via AMX Device Discovery / Control4 SDDP. Independently corroborates the transport and discovery findings above.
- [amosyuen/ha-epson-projector-link](https://github.com/amosyuen/ha-epson-projector-link) — a Home Assistant integration over TCP + ESC/VP21 for modern Epson home-cinema projectors; the closest peer to this backend's approach.
- [pszafer/epson_projector](https://github.com/pszafer/epson_projector) — the Python library (CGI / TCP / serial) the command vocabulary here was first derived from; its `projector_tcp.py` is the `ESC/VP.net` handshake reference.
- [mjaakkol/escvp21emulator](https://github.com/mjaakkol/escvp21emulator) — a Rust ESC/VP21 *emulator* (serial). An independent implementation that corroborates the reply framing (`VALUE\r:`) and the distinct power codes (`00` / `01` / `02` / `03`) modeled above; a useful manual reference, though serial-only (no `ESC/VP.net` TCP handshake, so not a drop-in peer for this backend's TCP transport).
- [`pjlink`](https://lib.rs/crates/pjlink) — a Rust PJLink client crate; the candidate dependency for the deferred PJLink transport.
