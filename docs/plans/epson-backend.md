# Epson projector backend + backend-developer guide + interface audit

## Context

`tisvlay` controls HDMI-connected AV devices over the network from a Linux desktop (the GPU has no HDMI-CEC). It ships an LG WebOS backend and a Mock backend behind a vendor-agnostic, four-trait core. We want a third backend — **Epson projectors** (target hardware: Epson Pro Cinema LS12000) — because a projector on a living-room PC is the same use case as a TV: remote network control to power on/off, switch inputs, and blank the light source to preserve operating hours (the projector analog of OLED burn-in protection).

This effort is deliberately three things at once, in the order you asked:

1. **Audit** the backend interface using the projector as the lens — projectors expose one genuine gap (multi-second, stateful warmup/cooldown transitions the power model can't represent) plus several mappings worth pinning down (A/V-mute = screen-off, laser-brightness = backlight, poll-only state).
2. **Write a backend-developer guide** so future contributors can add vendors — drafted first from the *existing* LG/Mock backends so it is a real artifact before Epson is built.
3. **Implement the Epson backend while abiding by that guide**, then **refine the guide** from the friction the build surfaces.

We have **no Epson hardware yet**. The deliverable is a backend fully tested against a fake server, plus docs seeded from research, with the on-hardware unknowns flagged in `docs/epson/` and resolved in a **later hardware-validation follow-up**.

## Decisions (from your answers + dated firmware / API / real-world integration research)

- **Power-state modeling: extend the rich `PowerState` enum (your steer — "LG already does more than on/off").** Add `WarmingUp`/`CoolingDown`, mirroring LG's transitional `ActiveStandby`. Verified zero compile-breakage (every `match` is `_`-covered or `matches!`-based).
- **Transport: ESC/VP21 over raw TCP port 3629 — with the Web API kept as a future transport behind the seam.** Dated research corrected an earlier stale "HTTP disabled" finding: the LS12000 *did* gain a Web Control browser UI in firmware v2.00, present and maintained through **v3.04 (2026-04-22)**. TCP 3629 is the right v1 choice anyway because it is **firmware-robust** (works on every firmware version, updated or not — no v2.00+ dependency) and the **richest** path: it is what the community's "best feature set" integration uses, and the only one that also reaches `LUMLEVEL` (backlight) and `MUTE` (screen-blank). PJLink (TCP 4352) works but is power+input-only. The command-layer / transport seam keeps the now-confirmed Web API (and PJLink/serial) as clean future additions.
- **No authentication in v1.** ESC/VP21 over TCP 3629 is unauthenticated (the `ESC/VP.net` handshake, no Digest/password). Pairing reduces to a connectivity check. (A future Web API or PJLink transport reintroduces an optional password — `EPSONWEB`/PJLink realms, no password by default — as a one-row `Secret` schema addition then.)
- **Capabilities: drop audio, add backlight.** The LS12000 is speakerless (audio routes through the AVR) and the API exposes no volume command, so **VOLUME and MUTE are out**. **BACKLIGHT is in, mapped to `LUMLEVEL`** (continuous laser-brightness) — it directly serves "dim to preserve laser hours". Your volume-emulated-mute idea (drive volume to 0, recall on unmute, latest-set-while-muted becomes the new recall) is kept as a **documented dev-guide pattern** (emulating a missing capability with daemon-side recall state), not implemented for Epson v1.
- **Discovery: deferred entirely to hardware.** Not in v1. When revisited, the LS12000's *documented* discovery is **PJLink / AMX Device Discovery / Control4 SDDP** (per its User's Guide), not mDNS/SSDP — a hardware-validation decision.
- **WoL wake (gated)** stays — optional `mac` + `supports_wake()`, reusing core `wol.rs`. Network `PWR ON` for `Communication On` standby, magic packet for deep standby.
- **Lamp/laser-hours deferred** to a future diagnostics idea doc (no `TvState` slot today). A future PJLink transport would provide it natively.

## What the research established (dated — firmware evolves, so timestamps matter)

Sources, current as of 2026: the LS12000 **firmware history** (latest **3.04**, 2026-04-22, file `EPSONPJ_HA47_WWV304.bin` — `HA47` = the LS12000 model code), the Web API spec v1.1.1 (2021), the LS12000 User's Guide (`cpd61119`), and **2023–2026 Home Assistant field reports**.

**Firmware timeline (this is where an earlier undated source misled the first pass):**
- Launch firmware (2022) had no web/HTTP control — early integration attempts saw "connection refused". *This was the stale fact I first trusted.*
- **v2.00** added **Web Control** — a browser UI plus *Basic Control* and a *Web Control Password*. A web/HTTP surface now exists.
- v2.01 (2025-06-10), v3.00 (2025-07-01), **v3.02 (2025-11-10)** — adds a wrong-password lockout "when connecting … via a web browser", i.e. the web UI is current and maintained — v3.03 (2026-02-02), **v3.04 (2026-04-22, latest)**.

**Current (2026) control surface, from dated HA field reports:**
- **PJLink** (TCP 4352) works but is **power + input-only** ("turning it on/off works, but that's it" — 2026-02-13). The PJLink password and the Web Control password are *separate* realms and trip people up.
- **Web Control** browser UI exists (auth realms `EPSONREMOTE` for Basic Control, `EPSONWEB` for the rest; no password by default).
- **ESC/VP21 over TCP 3629** is the **richest working programmatic path** — the community's "best feature set" integration (`amosyuen/ha-epson-projector-link`, recommended 2025-03-26) uses it; it is the only path that also reaches `LUMLEVEL` (backlight) and `MUTE` (screen-blank), not just power.
- **Standby Mode = Communication On** is required for network control in standby (and the wake path); standby draw 0.4 W (Communication Off) vs 2 W (On).

**The control vocabulary is ESC/VP21** across all transports — `PWR ON|OFF|?`, `SOURCE xx|?` (30=HDMI1, A0=HDMI2, 53=LAN), `MUTE ON|OFF|?` (A/V mute — blanks the image, the laser-saving feature), `LUMLEVEL n` (laser brightness, `0`=min — the backlight analog), `LUMINANCE` (laser power mode), `CMODE` (color mode). The TCP transport opens a socket to port 3629, does the `ESC/VP.net` handshake, sends `CMD\r`, and reads a `CMD=VALUE:`-style reply terminated by `:`.

**Power codes:** `PWR?` → `01`=on, `04`=standby, `02`/`00`=busy. Progression: `Standby(04)` → [`PWR ON`] → `Warming(02, ~30-40 s)` → `On(01)`; `On` → [`PWR OFF`] → `Cooling(~10 s)` → `Standby`. ESC/VP21's busy code does not distinguish warming from cooling (see nuance below); PJLink's power query does, which is one reason a future PJLink transport could sharpen the power model.

**No push/event/subscription API — confirmed authoritatively.** The Web API spec is pure REST request/response; its only `events` are *digital-signage playback schedules*, not state-change notifications. No WebSocket/SSE/long-poll/webhook anywhere. Epson's first-party app (**iProjection**) necessarily polls too. So poll-only is the confirmed reality, and the daemon's existing client-side poll fallback (when `subscribe_state()` is `NotSupported`) is the correct and only path.

### Reference implementations to review during build

- [`amosyuen/ha-epson-projector-link`](https://github.com/amosyuen/ha-epson-projector-link) — Epson Home Cinema over TCP + ESC/VP21; the community-recommended "best feature set" integration. Closest match to the target; worth cloning alongside the existing `pszafer` checkout.
- [`mag1024/epson-projector-modern-homeassistant`](https://github.com/mag1024/epson-projector-modern-homeassistant) — modern Epson (LS11000/LS12000) over a persistent TCP connection (archived Dec 2023; superseded by the above, but useful).
- [`pszafer/epson_projector`](https://github.com/pszafer/epson_projector) (already checked out locally) — the CGI/TCP/serial library; `projector_tcp.py` is the `ESC/VP.net` handshake reference.
- [`TheRealKillaruna/ha-pjlink2`](https://github.com/TheRealKillaruna/ha-pjlink2) — PJLink Class 2, tested against the LS12000; the reference if a PJLink transport is added later.

### Two projector nuances the design must handle

- **Warmup vs cooldown share the `02` busy code.** Direction is inferred from the **last power command** (`PWR ON` → `WarmingUp`, `PWR OFF` → `CoolingDown`); `EpsonClient` tracks in-flight power intent. *This inference is unfalsifiable without hardware* — the fake-server tests confirm the wiring, not that the firmware behaves this way. Flagged as a hardware-validation item, not "fully tested". (A future PJLink transport would replace the inference with native distinct codes.)
- **Screen-off has no power code of its own.** An A/V-muted projector still reports `PWR?=01`, so `EpsonConnection` derives `PowerState::ScreenOff` from `PWR?=01 AND MUTE?=on`, and `Active` from `PWR?=01 AND MUTE?=off` — the same compound derivation LG already does (`lg/backend.rs:384`). Verified the daemon's `screen_on`/`screen_off` reliable-dispatch is vendor-agnostic about how `ScreenOff` is computed, so no hazard.

## Milestones

Recommended PR boundaries: **M0 is its own vendor-neutral PR**; **M1+M2+M3 land together** (crate scaffold, first real usage, registration, and docs — no orphaned plumbing). Hardware validation is a separate later effort.

### M0 — Interface audit: warmup/cooldown power states (vendor-neutral)

Extend the shared power model so any backend can report transitional power honestly. Exercised through the **mock backend** so the new states have a first consumer and the D-Bus contract is regression-locked before any Epson byte.

| File | Change |
|------|--------|
| `crates/tisvlay-core/src/state.rs` | Add `WarmingUp`, `CoolingDown` to `PowerState` (before `Unknown`). `is_on()`: add `WarmingUp` (powering on → `true`); leave `CoolingDown` out (`false`). Both stay out of the `is_screen_on` whitelist. Add both rows to `update_power_screen_on_mapping_is_complete`. Add `PowerState::parse` arms (`"warming up"`/`"warmingup"` → `WarmingUp`, etc.) for completeness. |
| `crates/tisvlayd/src/power_state.rs` | Add `WARMING_UP`/`COOLING_DOWN` constants; add explicit `WarmingUp`/`CoolingDown` arms to `derive_power_state` (promoting them out of `_ => STANDBY`) and **prune the now-stale "temporarily conservative wildcard" comment** at `:63` (no tombstone pointer). **Deliberately decide `needs_wake_preflight` for both** — see note below; extend `derivation_table` + `needs_wake_preflight_table`. |
| `crates/tisvlay-gui/ui/components/tvs_pane.slint` | The steady-state branch (`:620`) interpolates the **raw** wire string (`@tr("Power state: {}", power-state)`), so a new token renders as literal "warming_up". Add a `@tr` ladder for `power-state` mapping `warming_up`/`cooling_down` (and the existing tokens) to human labels — mirror the adjacent `power-cycle-phase` ladder (`:614`). |
| `crates/tisvlay-gui/lang/tisvlay-gui.pot` | Run `just i18n-extract` after the `@tr` additions; commit the refreshed `.pot` diff (required by the i18n workflow). |
| `crates/tisvlay-tray/src/state.rs` | **Logic fix, not a label:** `is_on` at `:115` is a denylist (`!= "standby" && != "unknown"`), so `cooling_down` wrongly reads `is_on=true`. Exclude `cooling_down` (and confirm `warming_up` → `true` is intended). Add a test. Tray shows no power-state string, so no i18n here. |
| `crates/tisvlay-cli/src/main.rs` | No change — `list` emits raw JSON where `power_state` must stay machine-readable; not i18n-wired. |
| `crates/tisvlayd/tests/` (contract test) | Inject `WarmingUp`/`CoolingDown` via `MockHandle::inject_power_state` (the value path — `PowerState::parse` of a config string wouldn't know them), assert the `Tv1.Power.PowerState` cached property reads `warming_up`/`cooling_down`. |
| `docs/design.md` | Update the Power & Screen State Machine section + diagram with the two transient substates and their `(is_on, is_screen_on)` mapping; note the existing `PowerCyclePhase` progress signal complements them. Fix the stale "derives from `(is_on, is_screen_on)`" wording (`:287`) to name `derive_power_state` if that section is touched. |

**`needs_wake_preflight` is a behavioral decision, not a formality.** It is a negation whitelist (`!matches!(Active|ScreenSaver|ScreenOff)`), so new variants default to `true` = "wake first". For `CoolingDown`, `true` is right. For `WarmingUp`, `true` risks re-dispatching `power_on` at a projector that is *already* warming, with a verify loop polling for `Active` that `WarmingUp` never satisfies → escalation churn. The clean resolution: Epson's `wake_and_connect` **polls through warmup to `Active` before returning** (below), so the daemon almost never sees `WarmingUp` as a screen-op target; `WarmingUp` is observed mainly during *passive* (remote-initiated) warmup. Decide `WarmingUp`'s membership explicitly with a test that proves no escalation loop — the compile-clean wildcards make a wrong choice silent.

The existing `PowerCyclePhase` (`state.rs:26`, the `Tv1.Power.PowerCycleProgress` signal) gives staged feedback during a *daemon-driven* power-on; the new `PowerState` variants give the *steady substate* a passively-warming/cooling projector sits in. Complementary — and for Epson, `PowerCyclePhase::Connected` (the TCP socket connects during warmup) can precede `PowerState::Active`, which is exactly why the connector must poll to `Active`.

### M1 — Backend-developer guide (draft) + Epson crate skeleton

| File | Change |
|------|--------|
| `docs/backends/implementing-a-vendor.md` (new) | First draft from LG (real-world) + Mock (minimal), pointing at living code (see Documentation). New `docs/backends/` family. **Correct structure claim:** LG fuses `TvBackend`+`TvConnector`+`TvDiscovery` into `backend.rs` with a *separate* `connection.rs` for `TvConnection`; Mock splits connector/connection. Teach that either split is fine. |
| `Cargo.toml` (workspace) | Add `crates/tisvlay-epson` member + workspace dependency. |
| `crates/tisvlay-epson/Cargo.toml` (new) | Deps: `tisvlay-core`, `tokio` (TCP), `serde`/`toml`. No HTTP/Digest deps in v1 (TCP transport). |
| `crates/tisvlay-epson/src/lib.rs` (new) | Public exports + `VENDOR_ID = "epson"`. |
| `crates/tisvlay-epson/src/escvp21.rs` (new) | ESC/VP21 command + response codec; map `PWR?` `01/02/04` (+ MUTE state + last-power-intent) → `PowerState`. Pure, table-tested. Analog of LG `protocol.rs` + `endpoints.rs`. |
| `crates/tisvlay-epson/src/config.rs` (new) | `EpsonVendorConfig` + `config_schema()` rows (see below). |
| `crates/tisvlay-epson/src/transport.rs` (new) | TCP-3629 transport: connect, `ESC/VP.net` handshake, `CMD\r` write, read to the `:` terminator. Keep the command/transport boundary clean but `pub(crate)`/concrete for v1 — a single-impl public `trait` would draw `cargo workspace-unused-pub` flags; promote to a public seam when a second transport (Web API / PJLink / serial) lands. |
| `crates/tisvlay-epson/src/client.rs` (new) | `EpsonClient` — maps high-level ops to ESC/VP21; owns the warmup/cooldown poll loop + last-power-intent + reconnection (TCP socket drops on standby, like LG's WebSocket). Analog of LG `client.rs`. |
| `crates/tisvlay-epson/src/test_server.rs` (new, `#[cfg(test)] pub(crate)`) | `FakeEpsonServer` — `tokio` TCP responder speaking the `ESC/VP.net` handshake + ESC/VP21 `CMD=VALUE:` replies: canned replies, scriptable warmup/cooldown progression, connection-drop injection. Seam points the client at `127.0.0.1:0`. Mirrors LG `test_server.rs`. |

### M2 — Epson trait impls + tests + registration

| File | Change |
|------|--------|
| `crates/tisvlay-epson/src/backend.rs` (new) | `EpsonBackend` (`TvBackend`): `capabilities` = `POWER\|SCREEN\|INPUT\|BACKLIGHT\|WOL\|IPV4` (no VOLUME/MUTE/DISCOVERY — see Decisions); `verify_retry_config` override with a longer verify delay for projector latency. |
| `crates/tisvlay-epson/src/connector.rs` (new) | `EpsonConnector` (`TvConnector`): `transport()=Network`; `survives_source_ip_change()=false` (persistent TCP socket, like LG's WebSocket — a NIC shift breaks the 4-tuple → reconnect); `connect`; `supports_wake` (gated on `mac`); `wake`/`wake_and_connect` — network `PWR ON` for `Communication On` standby, WoL magic packet via core `wol.rs` for deep standby (the projector analog of LG's DirectConnect-vs-WolSent), **then poll `PWR?` through warmup to `Active` before returning** so action chains (`on_startup = power_on → switch_input`) don't fire mid-warmup; `start_pairing`/`build_config_from_pairing` (connectivity check, below). |
| `crates/tisvlay-epson/src/connection.rs` (new) | `EpsonConnection` (`TvConnection`): `get_state` (compose `PWR?`+`MUTE?`+`SOURCE?`+`LUMLEVEL?`); `power_off`; `screen_off`/`screen_on`/`is_screen_on` (`MUTE ON\|OFF\|?`); `switch_input`/`get_current_input`/`list_inputs`; `set_backlight`/`get_backlight` (`LUMLEVEL` — range confirmed on hardware); leave `subscribe_state`/volume/mute at `NotSupported`. |
| `crates/tisvlay-backends/Cargo.toml` | Add `tisvlay-epson` dep. No discovery feature in v1. |
| `crates/tisvlay-backends/src/lib.rs` | Add `register_epson()` (clone of `register_lg`, `:361`, with `discover_fn = None` since discovery is deferred) and call it in `create_registry` (`:418`); add it to `create_test_registry` for multi-vendor lookup; add an `epson` registry-test assertion. |
| `crates/tisvlay-epson/tests/` + inline | Integration vs `FakeEpsonServer` (power-on through warmup → `WarmingUp` then `Active`; screen via mute; input; backlight via LUMLEVEL; reconnect-on-drop; gated WoL); `escvp21` codec table tests; config-parse tests. |
| `docs/epson/{protocol.md,testing.md}` (new) | Seed from the real code/fixtures as written. |

### M3 — Refine the guide + finalize Epson docs + record the wire-format trigger

| File | Change |
|------|--------|
| `docs/backends/implementing-a-vendor.md` | Fold in friction the build surfaced (poll-only vs push, the connector/connection split with a warmup-poll task, TCP reconnection, the volume-emulated-mute pattern, capabilities that don't map — audio on a speakerless device). Add Epson as the third reference (minimal=Mock, network-push=LG, poll-only-TCP=Epson). |
| `docs/epson/firmware-quirks.md` (new) | Stub seeded with known-unknowns as explicit open questions (warming/cooling code distinctness, `LUMLEVEL` range/units, WoL + `Communication On` behavior, whether the v2.00+ Web API offers any capability edge, lamp-hours via a future PJLink path) in the LG Symptom→Root-Cause→Fix→Regression shape. |
| `docs/ideas/projector-lamp-hours-diagnostic.md` + `docs/ideas/poll-rate-during-power-transition.md` (new) | Deferred surfaces; plus index rows in `docs/ideas.md`. |
| `docs/ideas/asv-wire-format-migration.md` | Record the JSON-vs-`a{sv}` decision **inline** (recommend stay on JSON — one more vendor doesn't justify the churn), pointing at `design.md`. **No durable doc may link the ephemeral `docs/plans/epson-backend.md`** — guard against the dangling link on merge. |
| `docs/testing.md` | Add an "Epson backend testing" pointer section mirroring the LG one (`:127`). |
| `docs/design.md`, `.claude/CLAUDE.md`, `docs/discovery.md` | Cross-reference + structure edits (see Documentation). The design.md dependency-graph mermaid has a `samsung` placeholder node (`:69`) — add `epson`. |

### Follow-up (separate effort) — hardware validation on the LS12000

Probe the projector (firmware v3.04+): confirm TCP 3629 + the `ESC/VP.net` handshake, the real warming/cooling `PWR?` behavior, the `LUMLEVEL` range/units, WoL + `Communication On` standby behavior, and the discovery mechanism (PJLink / AMX / Control4 SDDP). Evaluate whether the v2.00+ Web API or PJLink is worth adding as a second transport (PJLink for clean power codes + lamp-hours; Web API for stateless IP-change resilience). Fill `docs/epson/firmware-quirks.md`; tune `verify_retry_config` + warmup poll cadence from measured timings; promote `protocol.md` "TBD" cells to verified.

## Key designs

### Config schema (`EpsonVendorConfig` + `config_schema()` rows)

| Field | Type | Visibility | Notes |
|-------|------|-----------|-------|
| `ip` | Ip | UserInput | Required, **first** row → the backend's primary descriptor (`backends/lib.rs:208`). `autofill: "discovery.ip"` (no-op until discovery lands). |
| `mac` | Mac | UserInput | Optional; enables WoL. |
| `model` | Text | Diagnostic | Manual entry; read-only INFO. (Confirm `PairingFieldType::Text` exists; LG uses `Ip/Mac/Password/Select`.) |

No `password` field — TCP 3629 ESC/VP21 is unauthenticated. (A future PJLink or Web API transport reintroduces an optional password as a `Secret`-visibility row then.) No reserved `port`/`endpoint` selector — default 3629; add when a second transport actually lands.

### Pairing / auth

Trivial. `start_pairing` opens TCP 3629, performs the `ESC/VP.net` handshake, and confirms the projector responds → emits `Complete` with empty credentials. No password, no `AwaitingUserInput`, no client-key handshake. This is the simplest end of the `PairingState` machine and a useful "no-auth vendor" worked example for the guide.

### Capabilities recap

`POWER | SCREEN | INPUT | BACKLIGHT | WOL | IPV4`. SCREEN = A/V-mute (`MUTE ON|OFF`) — blank the image to save the laser, mapped to `screen_off`/`screen_on`, no new bit. BACKLIGHT = `LUMLEVEL` (laser brightness 0..N → 0..100; range confirmed on hardware). No VOLUME/MUTE (speakerless unit; no volume in the API; the emulated-mute pattern is documented, not shipped). No DISCOVERY in v1. IPV6 omitted until tested.

### Transport + testing

`FakeEpsonServer` (mirroring LG's `FakeSsapServer`, see `docs/lg/testing.md`) makes the backend testable with no projector: a `tokio` TCP listener that answers the `ESC/VP.net` handshake and returns scripted `CMD=VALUE:` replies, drives the warmup/cooldown progression, and can drop the connection to exercise reconnection. The client's connection seam points at `127.0.0.1:0`. Layered: `escvp21` codec (table-driven) → config parse → client integration → backend-trait → fault injection. Caveat: the warmup-direction inference (last-power-intent) is validated for *wiring* only; its correctness against real firmware is a hardware-validation item.

## Documentation deliverables

### Guide — `docs/backends/implementing-a-vendor.md`

New `docs/backends/` family (topic subdir like `docs/lg/`). Mirrors `docs/i18n.md`'s shape and teaches by **pointing at the living Mock (minimal) and LG (real-world) code**, never pasting it. Sections: Architecture (→ `design.md` § Core Traits) · Crate structure (the file layout, with the corrected LG=Backend+Connector+Discovery-fused / Connection-separate vs Mock-split note) · Workflow: implement the four traits (referencing real Mock/LG methods; "override only what you support") · Capabilities, config schema, field visibility · Pairing & auth (the spectrum: Mock auto-accept → Epson no-auth-connectivity-check → LG interactive client-key) · Testing (fake-server pattern + harness layers) · Registering the backend (the one `tisvlay-backends` edit) · Vendor-specific docs (create `docs/<vendor>/`) · Decision rationale (opaque `toml::Value` config; schema-driven visibility; `discover()` returns `Vec` not `Stream`; per-backend `verify_retry_config`; poll-only satisfies the same reliability contract; **emulating a capability the device lacks via daemon-side recall state** — your volume-mute idea as the worked example).

### Epson docs — `docs/epson/` (mirrors `docs/lg/`)

`protocol.md` (ESC/VP21 vocab over TCP 3629, the `ESC/VP.net` handshake, power codes, warmup/cooldown timing, `LUMLEVEL`, the dated firmware/transport landscape, the no-push finding — `webos.md` analog, "verified?" cells TBD) · `firmware-quirks.md` (M3 stub of known-unknowns) · `testing.md` (`FakeEpsonServer`).

### Audit-findings homes

Durable contract decisions → `docs/design.md` (the `WarmingUp`/`CoolingDown` state machine, A/V-mute=SCREEN mapping, LUMLEVEL=BACKLIGHT mapping, poll-only handling, Epson `verify_retry_config` as the first non-LG tuning). Deferred surfaces → `docs/ideas/` + index rows (lamp-hours diagnostic, adaptive poll-during-transition).

### Cross-reference edits

`docs/design.md` (§ Dependency Graph link the guide; `tisvlay-epson` crate-table row; mermaid `epson` node) · `.claude/CLAUDE.md` (Reference Documentation list + guide + `docs/epson/`; Current Project Structure tree with per-file `tisvlay-epson/` rows incl. `test_server.rs`, and `docs/backends/`+`docs/epson/`) · `docs/ideas.md` (two new rows) · `docs/discovery.md` (optional: Epson as a vendor that defers discovery, with PJLink/AMX/Control4 as the eventual mechanism) · `docs/testing.md` (Epson pointer).

### Ephemeral repo plan

Per `docs/plans/README.md`, this plan (`docs/plans/epson-backend.md`) is the only place phase/step numbers are allowed; it tracks M0–M3 + the friction log. **Migrate and delete on merge** (architecture→`design.md`, quirks→`docs/epson/`, deferred→`docs/ideas/`), then grep for and remove any inbound links.

## Open questions / hardware-TBDs (absorbed by config + docs, not blocking)

Confirm TCP 3629 + `ESC/VP.net` handshake on firmware v3.04+ · whether `PWR?` distinguishes warming from cooling (else last-power-intent inference — unfalsifiable pre-hardware) · the `LUMLEVEL` range/units (for the 0..100 backlight mapping) · WoL + `Communication On` standby behavior · whether the v2.00+ Web API or PJLink is worth adding as a second transport · the discovery mechanism when revisited (PJLink / AMX / Control4 SDDP) · lamp-hours (deferred; native via a future PJLink path).

## Verification

- Workspace: `nix develop -c just check` (fmt + build + clippy + test) — capture once to a file and grep, don't re-run.
- Epson without hardware: M2 integration tests drive `EpsonClient` against `FakeEpsonServer` (power-on through `WarmingUp`→`Active`, screen-via-mute, input, backlight-via-LUMLEVEL, reconnect-on-drop, gated WoL).
- M0 contract: mock-injected `WarmingUp`/`CoolingDown` assert the `Tv1.Power.PowerState` property reads `warming_up`/`cooling_down`; the three updated power-state tables + the tray `is_on` test cover the bool logic; GUI label render verified via the `@tr` ladder.
- End-to-end on the session bus: `tisvlayd --session`; `busctl --user call … Manager1 GetSupportedVendors` shows `epson`; `GetPairingSchema s epson` returns the schema; add a fake/real Epson and watch `busctl --user monitor` for the transitions.
- Before handoff: `nix develop -c prek run -a` (commit the refreshed `.pot` diff).

## Proposed commit sequence

- **PR 1 (M0):** `feat(core): model projector warmup/cooldown as PowerState variants` — the variants, daemon D-Bus strings (pruning the stale wildcard comment), the GUI `@tr` ladder + refreshed `.pot`, the tray `is_on` logic fix + test, the mock-driven contract test, the `design.md` state-machine update. PR body notes the i18n extract.
- **PR 2 (M1+M2+M3), ordered so the docs-first→implement→refine loop shows in history, and the crate is never on `main` unregistered:**
  1. `docs(backends): draft the vendor-backend implementation guide` (from LG/Mock).
  2. `feat(epson): scaffold tisvlay-epson with ESC/VP21 over TCP 3629 + fake server, and register the vendor` (scaffold + `register_epson` together).
  3. `feat(epson): implement the four backend traits (power/screen/input/backlight + gated WoL)`.
  4. `docs(epson): seed protocol/testing/quirks docs and refine the backend guide`.
