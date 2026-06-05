# Epson projector backend + backend-developer guide + interface audit

## Context

`tisvlay` controls HDMI-connected AV devices over the network from a Linux desktop (the GPU has no HDMI-CEC). It ships an LG WebOS backend and a Mock backend behind a vendor-agnostic, four-trait core. We want a third backend — **Epson projectors** (target hardware: Epson Pro Cinema LS12000) — because a projector on a living-room PC is the same use case as a TV: remote network control to power on/off, switch inputs, and blank the light source to preserve operating hours (the projector analog of OLED burn-in protection).

This effort is deliberately three things at once, in the order you asked:

1. **Audit** the backend interface using the projector as the lens — projectors expose one genuine gap (multi-second, stateful warmup/cooldown transitions the power model can't represent) plus several mappings worth pinning down (A/V-mute = screen-off, Digest password auth, poll-only state).
2. **Write a backend-developer guide** so future contributors can add vendors — drafted first from the *existing* LG/Mock backends so it is a real artifact before Epson is built.
3. **Implement the Epson backend while abiding by that guide**, then **refine the guide** from the friction the build surfaces.

We have **no Epson hardware yet**. The deliverable is a backend fully tested against a fake server, plus docs seeded from research, with the on-hardware unknowns flagged in `docs/epson/` and resolved in a **later hardware-validation follow-up**.

## Decisions (from your answers, refined by a design/correctness/maintainability review)

- **Power-state modeling: extend the rich `PowerState` enum (your steer — "LG already does more than on/off").** Add `WarmingUp`/`CoolingDown`, mirroring LG's transitional `ActiveStandby`. Verified zero compile-breakage (every `match` is `_`-covered or `matches!`-based).
- **First HTTP transport: the Web API** (`/api/v01/control/escvp21`, Digest `EPSONWEB`) — the LS12000-documented path. CGI/TCP stay additive behind a command/transport boundary.
- **v1 optional surface: WoL wake (gated), audio volume, and a discovery probe.** Two refinements the review forced, flagged here so they are visible for approval:
  - **Audio *mute* is dropped as a separate capability** (you selected "volume + mute"). On Epson, ESC/VP21 `MUTE` *is* A/V-mute — the same command already mapped to `screen_off` — and A/V-mute silences audio anyway. Declaring a distinct `Tv1.Mute` control would issue the identical `MUTE ON` byte and stomp `Tv1.Screen` state. So **VOLUME is in; standalone MUTE is out**, and "mute the room" is served by screen-blank. A separate audio-only mute is a hardware-TBD (re-add the cap only if the firmware exposes one). This *is* an audit finding worth teaching in the guide.
  - **Discovery primary mechanism switches from SSDP to PJLink UDP search.** PJLink (UDP 4352) is the projector-industry-standard and broadly implemented by Epson; SSDP is far less universal on projectors than on TVs. Using PJLink *only for discovery* (not control) does not contradict your preference for HTTP/REST control. SSDP/mDNS are documented fallbacks. Discovery remains the highest hardware-risk item and is gated behind a feature.
- **Lamp/laser-hours deferred** to a future diagnostics idea doc (no `TvState` slot today).

## What the research established

The Python reference (`/home/ruffsl/ws/lgtv/libs/epson_projector`, the Home Assistant `pszafer/epson_projector`) confirmed:

- All transports (HTTP Web API / legacy CGI, raw TCP `ESC/VP.net` 3629, RS-232) speak **ESC/VP21** — `PWR ON|OFF|?`, `SOURCE=xx|?` (30=HDMI1, A0=HDMI2, 53=LAN), `MUTE ON|OFF|?` (A/V mute — blanks the image, the bulb-saving feature), `VOL?` + volume keys, `KEY xx`. This is why a shared **ESC/VP21 command layer** sits above a swappable **transport**.
- **Power codes:** `PWR?` → `01`=on, `04`=standby, `02`/`00`=busy. Progression: `Standby(04)` → [`PWR ON`] → `Warming(02, ~30-40 s)` → `On(01)`; `On` → [`PWR OFF`] → `Cooling(~10 s)` → `Standby`. The library hides the busy window behind a 40 s/10 s lock; we expose it as real state.
- **Auth:** HTTP Digest, user `EPSONWEB` + a user-supplied password; TCP/serial unauthenticated. No accept-on-device handshake.
- **Poll-only:** no push channel. The daemon already polls (client-side) when `subscribe_state()` is `NotSupported`.

### Two projector nuances the design must handle

- **Warmup vs cooldown share the `02` busy code.** Direction is inferred from the **last power command** (`PWR ON` → `WarmingUp`, `PWR OFF` → `CoolingDown`); `EpsonClient` tracks in-flight power intent. *This inference is unfalsifiable without hardware* — the fake-server tests confirm the wiring, not that the firmware behaves this way. Flagged as a hardware-validation item, not "fully tested".
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

The existing `PowerCyclePhase` (`state.rs:26`, the `Tv1.Power.PowerCycleProgress` signal) gives staged feedback during a *daemon-driven* power-on; the new `PowerState` variants give the *steady substate* a passively-warming/cooling projector sits in. Complementary — and for Epson, `PowerCyclePhase::Connected` (HTTP answers during warmup) can precede `PowerState::Active`, which is exactly why the connector must poll to `Active`.

### M1 — Backend-developer guide (draft) + Epson crate skeleton

| File | Change |
|------|--------|
| `docs/backends/implementing-a-vendor.md` (new) | First draft from LG (real-world) + Mock (minimal), pointing at living code (see Documentation). New `docs/backends/` family. **Correct structure claim:** LG fuses `TvBackend`+`TvConnector`+`TvDiscovery` into `backend.rs` with a *separate* `connection.rs` for `TvConnection`; Mock splits connector/connection. Teach that either split is fine. |
| `Cargo.toml` (workspace) | Add `crates/tisvlay-epson` member + workspace dependency. |
| `crates/tisvlay-epson/Cargo.toml` (new) | Deps: `tisvlay-core`, an async HTTP client with Digest + rustls (recommend `reqwest`), `serde`/`toml`, `tokio`. `discovery` feature gating the discovery module (mirrors `tisvlay-lg`). |
| `crates/tisvlay-epson/src/lib.rs` (new) | Public exports + `VENDOR_ID = "epson"`. |
| `crates/tisvlay-epson/src/escvp21.rs` (new) | ESC/VP21 command + response codec; map `PWR?` `01/02/04` (+ MUTE state + last-power-intent) → `PowerState`. Pure, table-tested. Analog of LG `protocol.rs` + `endpoints.rs`. |
| `crates/tisvlay-epson/src/config.rs` (new) | `EpsonVendorConfig` + `config_schema()` rows (see below). |
| `crates/tisvlay-epson/src/transport.rs` (new) | Web API transport (`/api/v01/control/escvp21?cmd=…`, Digest `EPSONWEB`), **per-request/connectionless** (no pooled keep-alive — load-bearing for `survives_source_ip_change`). Keep the command/transport boundary clean but `pub(crate)`/concrete for v1 — a single-impl public `trait` would draw `cargo workspace-unused-pub` flags; promote to a public seam when CGI/TCP land. |
| `crates/tisvlay-epson/src/client.rs` (new) | `EpsonClient` — maps high-level ops to ESC/VP21; owns the warmup/cooldown poll loop + last-power-intent. Analog of LG `client.rs`. |
| `crates/tisvlay-epson/src/test_server.rs` (new, `#[cfg(test)] pub(crate)`) | `FakeEpsonServer` — `tokio` HTTP responder: canned replies, scriptable warmup/cooldown progression, Digest 401. Connection seam points the client at `127.0.0.1:0`. Mirrors LG `test_server.rs`. |

### M2 — Epson trait impls + tests + registration

| File | Change |
|------|--------|
| `crates/tisvlay-epson/src/backend.rs` (new) | `EpsonBackend` (`TvBackend`): `capabilities` = `POWER\|SCREEN\|INPUT\|VOLUME\|WOL\|DISCOVERY\|IPV4` (no MUTE — see Decisions); `verify_retry_config` override with a longer verify delay for projector latency. |
| `crates/tisvlay-epson/src/connector.rs` (new) | `EpsonConnector` (`TvConnector`): `transport()=Network`; `survives_source_ip_change()=true` (connectionless HTTP — confirm no pooled socket); `connect`; `supports_wake` (gated on `mac`); `wake`/`wake_and_connect` — network `PWR ON` for network-standby, WoL magic packet via core `wol.rs` for deep standby (the projector analog of LG's DirectConnect-vs-WolSent), **then poll `PWR?` through warmup to `Active` before returning** so action chains (`on_startup = power_on → switch_input`) don't fire mid-warmup; `start_pairing`/`build_config_from_pairing` (Digest, below). |
| `crates/tisvlay-epson/src/connection.rs` (new) | `EpsonConnection` (`TvConnection`): `get_state` (compose `PWR?`+`MUTE?`+`SOURCE?`+`VOL?`); `power_off`; `screen_off`/`screen_on`/`is_screen_on` (`MUTE ON|OFF|?`); `switch_input`/`get_current_input`/`list_inputs`; `set_volume` (best-effort)/`get_volume`; leave `subscribe_state`/`set_backlight`/mute at `NotSupported`. |
| `crates/tisvlay-epson/src/discovery.rs` (new, `#[cfg(feature="discovery")]`) | `EpsonDiscovery` (`TvDiscovery`) via **PJLink UDP search** (small new infra), SSDP/mDNS documented as fallbacks. Returns SSDP-shaped `DiscoveredTv` so the daemon's MAC-enrichment/autofill plumbing applies. Highest hardware risk — mechanism may change after probing. |
| `crates/tisvlay-backends/Cargo.toml` | Add `tisvlay-epson` dep; thread an `epson-discovery` feature → `tisvlay-epson/discovery`. |
| `crates/tisvlay-backends/src/lib.rs` | Add `register_epson()` (clone of `register_lg`, `:361`) — **including the `#[cfg_attr(not(feature="discovery"), expect(unused_variables, …))]`** on the discovery param or clippy `-D warnings` fails when discovery is off — and call it in `create_registry` (`:418`); add it to `create_test_registry` for multi-vendor lookup; add an `epson` registry-test assertion. |
| `crates/tisvlayd/Cargo.toml` | Thread `epson-discovery` into the daemon's `discovery` default feature so Epson discovery actually runs in the shipped binary (mirrors how `discovery` → `tisvlay-backends/discovery`). |
| `crates/tisvlay-epson/tests/` + inline | Integration vs `FakeEpsonServer` (power-on through warmup → `WarmingUp` then `Active`; screen via mute; input; volume; Digest 401; gated WoL); `escvp21` codec table tests; config-parse tests. |
| `docs/epson/{protocol.md,testing.md}` (new) | Seed from the real code/fixtures as written. |

### M3 — Refine the guide + finalize Epson docs + record the wire-format trigger

| File | Change |
|------|--------|
| `docs/backends/implementing-a-vendor.md` | Fold in friction the build surfaced (poll-only vs push, the connector/connection split with a warmup-poll task, Digest collected-but-not-echoed vs `InputType`, the A/V-mute-vs-audio-mute collision as a worked "we tried, it aliased, we dropped the cap" lesson). Add Epson as the third reference (minimal=Mock, network-push=LG, poll-only+Digest=Epson). |
| `docs/epson/firmware-quirks.md` (new) | Stub seeded with known-unknowns as explicit open questions (which HTTP endpoint the LS12000 serves, whether auth is required, warming/cooling code distinctness, lamp-hours query, WoL/standby modes) in the LG Symptom→Root-Cause→Fix→Regression shape. |
| `docs/ideas/projector-lamp-hours-diagnostic.md` + `docs/ideas/poll-rate-during-power-transition.md` (new) | Deferred surfaces; plus index rows in `docs/ideas.md`. |
| `docs/ideas/asv-wire-format-migration.md` | Record the JSON-vs-`a{sv}` decision **inline** (recommend stay on JSON — one more vendor doesn't justify the churn), pointing at `design.md`. **No durable doc may link the ephemeral `docs/plans/epson-backend.md`** — guard against the dangling link on merge. |
| `docs/testing.md` | Add an "Epson backend testing" pointer section mirroring the LG one (`:127`). |
| `docs/design.md`, `.claude/CLAUDE.md`, `docs/discovery.md` | Cross-reference + structure edits (see Documentation). The design.md dependency-graph mermaid has a `samsung` placeholder node (`:69`) — add `epson`. |

### Follow-up (separate effort) — hardware validation on the LS12000

Probe the projector: which endpoint/auth the firmware serves, the real warming/cooling `PWR?` codes, the discovery mechanism, lamp-hours availability, WoL + standby-mode behavior. Fill `docs/epson/firmware-quirks.md`; tune `verify_retry_config` + warmup poll cadence from measured timings; promote `protocol.md` "TBD" cells to verified.

## Key designs

### Config schema (`EpsonVendorConfig` + `config_schema()` rows)

| Field | Type | Visibility | Notes |
|-------|------|-----------|-------|
| `ip` | Ip | UserInput | Required, **first** row → the backend's primary descriptor (`backends/lib.rs:208`). `autofill: "discovery.ip"`. |
| `password` | Password | UserInput | Collected (masked) in the wizard; persisted secret. See the collection-vs-display note below. |
| `mac` | Mac | UserInput | Optional; enables WoL. `autofill: "discovery.mac"`. |
| `model` | Text | Diagnostic | From discovery or manual entry; read-only INFO. (Confirm `PairingFieldType::Text` exists; LG uses `Ip/Mac/Password/Select`.) |

The reserved `endpoint` selector is **dropped from v1** — a single-option `Select` is dead config and orphaned plumbing; add it (one schema row) when CGI/TCP actually land.

**Password collection vs display is a genuine modeling detail to resolve in `pairing_schema.rs`.** The password must be (1) collected from the user during pairing, (2) masked, (3) *not* echoed in the INFO/diagnostics panes, (4) persisted at rest. `Secret` visibility satisfies (3)/(4) but is never collected; `UserInput` is collected but is shown in INFO. Determine whether `UserInput` + `field_type: Password` already suppresses INFO echo, or whether this needs a small visibility refinement — itself a clean audit/guide finding.

### Pairing / auth

No accept-on-device handshake. The wizard collects the password upfront (schema form); `start_pairing` validates it with a probe request and emits `Complete` immediately (no interactive hop in the common case). A projector with no password completes with empty credentials. If a projector unexpectedly challenges (401) without a supplied password, fall back to `PairingState::awaiting_user_input(…, InputType::Password)` (the struct variant `{ prompt, input_type }`). This exercises the low-friction end of the existing machine; if Digest needs a dedicated `InputType`, that extension point is captured in the guide.

### Capabilities recap

`POWER | SCREEN | INPUT | VOLUME | WOL | DISCOVERY | IPV4`. SCREEN = A/V-mute (`MUTE ON|OFF`) — the bulb-saving feature mapped to `screen_off`/`screen_on`, no new bit. **No standalone MUTE** (collides with SCREEN; A/V-mute already silences audio). BACKLIGHT omitted (no 0-100 pixel-brightness analog; `LUMINANCE` eco/normal noted as a possible future coarse mapping). IPV6 omitted until tested.

### Transport + testing

`FakeEpsonServer` (mirroring LG's `FakeSsapServer`, see `docs/lg/testing.md`) makes the backend testable with no projector: scripts the warmup/cooldown progression and Digest challenge; the client's connection seam points at `127.0.0.1:0`. Layered: `escvp21` codec (table-driven) → config parse → client integration → backend-trait → fault injection. Caveat: the warmup-direction inference (last-power-intent) is validated for *wiring* only; its correctness against real firmware is a hardware-validation item.

## Documentation deliverables

### Guide — `docs/backends/implementing-a-vendor.md`

New `docs/backends/` family (topic subdir like `docs/lg/`). Mirrors `docs/i18n.md`'s shape and teaches by **pointing at the living Mock (minimal) and LG (real-world) code**, never pasting it. Sections: Architecture (→ `design.md` § Core Traits) · Crate structure (the file layout, with the corrected LG=Backend+Connector+Discovery-fused / Connection-separate vs Mock-split note) · Workflow: implement the four traits (referencing real Mock/LG methods; "override only what you support") · Capabilities, config schema, field visibility · Pairing & auth (the spectrum: Mock auto-accept → LG interactive client-key → Epson Digest-password → no-auth) · Testing (fake-server pattern + harness layers) · Registering the backend (the one `tisvlay-backends` edit, incl. the `expect(unused_variables)` attr) · Vendor-specific docs (create `docs/<vendor>/`) · Decision rationale (opaque `toml::Value` config; schema-driven visibility; `discover()` returns `Vec` not `Stream`; per-backend `verify_retry_config`; poll-only satisfies the same reliability contract; the A/V-mute-vs-audio-mute lesson).

### Epson docs — `docs/epson/` (mirrors `docs/lg/`)

`protocol.md` (ESC/VP21 vocab, transports, power codes, warmup/cooldown timing, Digest — `webos.md` analog, "verified?" cells TBD) · `firmware-quirks.md` (M3 stub of known-unknowns) · `testing.md` (`FakeEpsonServer`).

### Audit-findings homes

Durable contract decisions → `docs/design.md` (the `WarmingUp`/`CoolingDown` state machine, A/V-mute=SCREEN mapping, poll-only handling, Epson `verify_retry_config` as the first non-LG tuning). Deferred surfaces → `docs/ideas/` + index rows (lamp-hours diagnostic, adaptive poll-during-transition).

### Cross-reference edits

`docs/design.md` (§ Dependency Graph link the guide; `tisvlay-epson` crate-table row; mermaid `epson` node) · `.claude/CLAUDE.md` (Reference Documentation list + guide + `docs/epson/`; Current Project Structure tree with per-file `tisvlay-epson/` rows incl. `test_server.rs` + feature-gated `discovery.rs`, and `docs/backends/`+`docs/epson/`) · `docs/ideas.md` (two new rows) · `docs/discovery.md` (optional: Epson as a non-SSDP discovery example) · `docs/testing.md` (Epson pointer).

### Ephemeral repo plan

Per `docs/plans/README.md`, this plan (`docs/plans/epson-backend.md`) is the only place phase/step numbers are allowed; it tracks M0–M3 + the friction log. **Migrate and delete on merge** (architecture→`design.md`, quirks→`docs/epson/`, deferred→`docs/ideas/`), then grep for and remove any inbound links.

## Open questions / hardware-TBDs (absorbed by config + docs, not blocking)

Which HTTP endpoint the LS12000 serves and whether auth is required (the Digest path + a probe handle both) · whether `PWR?` distinguishes warming from cooling (else last-power-intent inference — unfalsifiable pre-hardware) · the discovery mechanism (PJLink-UDP primary; may switch) · absolute `set_volume` vs up/down stepping · whether a distinct audio-mute exists · WoL + standby-mode behavior · lamp-hours query (deferred).

## Verification

- Workspace: `nix develop -c just check` (fmt + build + clippy + test) — capture once to a file and grep, don't re-run.
- Epson without hardware: M2 integration tests drive `EpsonClient` against `FakeEpsonServer` (power-on through `WarmingUp`→`Active`, screen-via-mute, input, volume, Digest 401, gated WoL).
- M0 contract: mock-injected `WarmingUp`/`CoolingDown` assert the `Tv1.Power.PowerState` property reads `warming_up`/`cooling_down`; the three updated power-state tables + the tray `is_on` test cover the bool logic; GUI label render verified via the `@tr` ladder.
- End-to-end on the session bus: `tisvlayd --session`; `busctl --user call … Manager1 GetSupportedVendors` shows `epson`; `GetPairingSchema s epson` returns the schema; add a fake/real Epson and watch `busctl --user monitor` for the transitions.
- Before handoff: `nix develop -c prek run -a` (includes `i18n-extract`-relevant checks; commit the `.pot` diff).

## Proposed commit sequence

- **PR 1 (M0):** `feat(core): model projector warmup/cooldown as PowerState variants` — the variants, daemon D-Bus strings (pruning the stale wildcard comment), the GUI `@tr` ladder + refreshed `.pot`, the tray `is_on` logic fix + test, the mock-driven contract test, the `design.md` state-machine update. PR body notes the i18n extract.
- **PR 2 (M1+M2+M3), ordered so the docs-first→implement→refine loop shows in history, and the crate is never on `main` unregistered:**
  1. `docs(backends): draft the vendor-backend implementation guide` (from LG/Mock).
  2. `feat(epson): scaffold tisvlay-epson with ESC/VP21 + Web API transport + fake server, and register the vendor` (scaffold + `register_epson` together).
  3. `feat(epson): implement the four backend traits and wire discovery + WoL`.
  4. `docs(epson): seed protocol/testing/quirks docs and refine the backend guide`.
