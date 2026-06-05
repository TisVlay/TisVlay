# Implementing a vendor backend

A *vendor backend* teaches `tisvlay` how to control one family of devices (LG WebOS TVs, Epson projectors, …). It is one crate that implements four traits from `tisvlay-core` plus a single registration line — no daemon, CLI, GUI, or session changes. This guide is the *how-to*; [docs/design.md](../design.md) § Core Traits is the authoritative *contract* (trait signatures, the D-Bus surface, the config schema). It teaches by pointing at the three living reference backends rather than pasting code that would drift, bracketing the range you might land in:

- [`tisvlay-mock`](../../crates/tisvlay-mock) — the **minimal** template you copy from (in-memory, auto-accept pairing, push via an in-process channel).
- [`tisvlay-lg`](../../crates/tisvlay-lg) — the **network-push** real-world example (WebSocket + SSAP, interactive accept-on-device pairing with a returned client key, a real `subscribe_state()` push channel, feature-gated SSDP discovery).
- [`tisvlay-epson`](../../crates/tisvlay-epson) — the **poll-only, no-auth** example (ESC/VP21 over a raw TCP socket, unauthenticated pairing reduced to a connectivity check, no push channel so the daemon polls, no discovery in v1).

Read the trait definitions in [`crates/tisvlay-core/src/traits.rs`](../../crates/tisvlay-core/src/traits.rs) alongside this guide — that file is the source of truth for every signature named below.

## Architecture

The core is vendor- and transport-agnostic. It knows four traits and nothing about how any device actually talks:

- **`TvBackend`** — static metadata: `vendor_id()`, `vendor_name()`, `capabilities()`, and the reliable-dispatch tuning knob `verify_retry_config()`. One trait object per vendor, consulted by the daemon for capability gating.
- **`TvConnector`** — the factory and lifecycle owner: `transport()`, `survives_source_ip_change()`, `connect()`, `wake()` / `wake_and_connect()` / `supports_wake()`, `start_pairing()` / `build_config_from_pairing()`, and the `config_schema()` that drives every UI form. Most of the backend surface lives here.
- **`TvConnection`** — a live, per-device control handle: power, screen, volume, mute, input, backlight, and an optional `subscribe_state()` push channel. **Every feature method defaults to `Err(NotSupported)`** — a backend overrides only what its hardware does.
- **`TvDiscovery`** — an optional, bounded `discover(timeout) -> Vec<DiscoveredTv>` scan. Backends with no discovery protocol simply don't implement it (and omit `Capabilities::DISCOVERY`).

The single composition point is [`tisvlay-backends`](../../crates/tisvlay-backends) — the *only* crate that imports vendor crates. Everything downstream (`tisvlayd`, CLI, GUI, session, tray) depends on `tisvlay-backends` and stays vendor-blind. The whole vendor config travels as an opaque `toml::Value`; only your crate ever deserializes it.

## Crate structure

There is no mandated file layout — only the four traits must be implemented somewhere in your crate. The three references bracket the sensible range:

| Concern | `tisvlay-mock` (minimal) | `tisvlay-lg` (network-push) | `tisvlay-epson` (poll-only TCP) |
|---------|--------------------------|-----------------------------|---------------------------------|
| `TvBackend` impl | `backend.rs` | `backend.rs` | `backend.rs` |
| `TvConnector` impl | `connector.rs` | `backend.rs` | `connector.rs` |
| `TvConnection` impl | `connection.rs` | `backend.rs` | `connection.rs` |
| `TvDiscovery` impl | `discovery.rs` | `backend.rs` (feature-gated) | (none in v1) |
| Vendor config struct | `config.rs` | `config.rs` | `config.rs` |
| Wire protocol / transport | (in-memory `state.rs`) | `client.rs`, `connection.rs`, `protocol.rs`, `endpoints.rs` | `escvp21.rs` (codec), `transport.rs` (TCP), `client.rs` (sequencing) |
| Fake server for tests | (driven via `handle.rs`) | `test_server.rs` (`#[cfg(test)]`) | `test_server.rs` (`#[cfg(test)]`) |
| `lib.rs` | `pub use` re-exports + `VENDOR_ID` + factory fns | same | same |

**Mock puts one trait impl per file** — the clearest layout to learn from; copy it when starting. **LG puts all four trait impls in `backend.rs`** and reserves the other modules for its substantial SSAP-over-WebSocket protocol machinery — a fine grouping when the wire protocol is large. **Epson splits per-trait like Mock** but factors its wire protocol into a pure, I/O-free codec (`escvp21.rs`, exhaustively table-tested) separate from the concrete TCP transport (`transport.rs`) and the high-level command sequencing (`client.rs`) — a clean split worth copying when your protocol has a non-trivial grammar but you still want each file readable. Pick whichever keeps each file readable; the daemon can't tell the difference. Every crate exposes its `VENDOR_ID` / `VENDOR_NAME` constants and a constructor from `lib.rs` (see [`tisvlay-mock/src/lib.rs`](../../crates/tisvlay-mock/src/lib.rs) `new_production_parts()` / `new_test_parts()`).

## Implementing the four traits

Work in this order; lean on the mock as a skeleton and the LG impl when you need the real-network version.

1. **`TvBackend`** — return your `vendor_id` / `vendor_name` and OR together the `Capabilities` bits your hardware supports (next section). Override `verify_retry_config()` only if your transport's latency differs materially from the LG-tuned default. See `MockBackend` / `LgBackend`.

2. **`TvConnector`** — the heart of the backend.
   - `transport()` returns `Transport::{Network, Serial, Usb, Cec}`; the daemon scopes carrier/route events to `Network` backends via `is_network_dependent()`.
   - `survives_source_ip_change()` declares whether your protocol survives the client's source IP changing on a NIC failover — `false` for a persistent socket (WebSocket, raw TCP), `true` for a stateless request-per-command transport.
   - `connect()` makes one fail-fast attempt and returns a `Box<dyn TvConnection>`. `wake_and_connect()` owns *all* retry/backoff/probe logic for the wake path — the daemon calls it and treats it as atomic. `supports_wake()` gates Wake-on-LAN per device (e.g. on whether a MAC is configured). Reuse the generic [`tisvlay_core::wol`](../../crates/tisvlay-core/src/wol.rs) helpers rather than re-inlining the send loop — `wake_with_retry(mac, max_attempts)` for the initial send (it backs off while the local NIC recovers after resume) and `wake_best_effort(mac)` for a single resend inside a connect-retry loop.
   - `start_pairing()` / `build_config_from_pairing()` drive the pairing state machine (see § Pairing and authentication).
   - `config_schema()` declares every vendor-config field once; the framework projects it into the pairing wizard, the INFO pane, and config display (see § Capabilities, config schema, and field visibility).

3. **`TvConnection`** — override the feature methods your device supports (`power_off`, `get_power_state`, `screen_on` / `screen_off` / `is_screen_on`, `set_volume` / `set_mute`, `switch_input` / `list_inputs`, `set_backlight`, …). Leave the rest at their `NotSupported` defaults — a speakerless projector simply never overrides volume/mute. `get_state()` returns a full [`TvState`](../../crates/tisvlay-core/src/state.rs) snapshot. If your protocol pushes asynchronous updates, implement `subscribe_state()` to return an `mpsc` receiver of cumulative `TvState` snapshots (LG does); if it is **poll-only, leave `subscribe_state()` unimplemented and the daemon's periodic poll feeds the same cached-state channel** — the reliability machinery never learns the difference (Epson is poll-only; ESC/VP21 has no push channel). Map your device's raw power substates onto the `PowerState` enum — including the transient `WarmingUp` / `CoolingDown` for warmup/cooldown-gated devices (see [docs/design.md](../design.md) § Power & Screen State Machine). When a substate has no native code of its own — an A/V-muted projector still reports "power on" — derive it from a *compound* read (Epson computes `ScreenOff` from `PWR?=on AND MUTE?=on`, the same compound derivation LG does), so the daemon's vendor-agnostic `screen_*` dispatch sees a clean `ScreenOff`.

4. **`TvDiscovery`** (optional) — implement `discover(timeout) -> Vec<DiscoveredTv>` only if you have a discovery protocol, and feature-gate it (LG gates SSDP behind a `discovery` feature). Return SSDP-shaped `DiscoveredTv` entries so the daemon's post-discovery MAC enrichment and `autofill: "discovery.<key>"` wizard pre-fill apply. See [docs/discovery.md](../discovery.md) § Trait boundary for why this is a bounded `Vec`, not a stream.

## Capabilities, config schema, and field visibility

`Capabilities` ([`crates/tisvlay-core/src/capabilities.rs`](../../crates/tisvlay-core/src/capabilities.rs)) is a bitflag set the CLI/GUI use to gate affordances: `POWER`, `SCREEN`, `VOLUME`, `MUTE`, `INPUT`, `BACKLIGHT`, `WOL`, `DISCOVERY`, plus the transport-family bits `IPV4` / `IPV6` (deliberately *not* in `STOCK` — opt in per family you have actually verified). Declare a bit only when the matching `TvConnection` method really works; clients trust the bits.

Your vendor config is a serde struct (your `config.rs`) deserialized from the opaque `[tv.<vendor>]` `toml::Value`. `config_schema()` returns a `Vec<PairingField>` describing each field, and each field carries a `FieldVisibility`:

- **`UserInput`** — the user types it; shown in the pairing wizard and the INFO pane, and editable.
- **`Diagnostic`** — backend-discovered or read-only; shown in the INFO pane only.
- **`Secret`** — credentials; filtered from every display and never sent to the GUI.

Declare the schema once and the framework projects it everywhere — adding an editable field is one schema row, with no D-Bus, CLI, or GUI rendering changes. Order the schema so the most informative `UserInput` field comes first; it becomes the backend's "primary descriptor" (`backends/lib.rs` `primary_descriptor`). See [docs/design.md](../design.md) § Schema-driven vendor config for the full model.

## Pairing and authentication

The same `PairingState` machine ([`crates/tisvlay-core/src/pairing.rs`](../../crates/tisvlay-core/src/pairing.rs): `AwaitingTvPrompt`, `AwaitingUserInput { input_type }`, `Complete { credentials }`, `Failed`) spans a wide range of auth models. The three references show the spectrum:

- **No real auth (trivial).** The mock auto-accepts via a config shorthand. A backend whose transport is unauthenticated validates connectivity in `start_pairing()` and emits `Complete` with **empty credentials** immediately — the simplest end of the machine. Epson is the worked example: ESC/VP21 over raw TCP has no auth, so its `start_pairing()` ([`connector.rs`](../../crates/tisvlay-epson/src/connector.rs)) opens the socket, performs the `ESC/VP.net` handshake to confirm the projector responds, and races that against a cancel command — "pairing" *is* a reachability check. The empty credential is a deliberate marker for the no-auth flow that `build_config_from_pairing()` reads.
- **Interactive accept-on-device with a returned credential.** LG races a `tokio::select!` between the TV's user-approval handshake and a cancel command, captures the returned client key, and stores it via `build_config_from_pairing()`. Uses `AwaitingTvPrompt`.
- **A user-supplied secret (password / PIN).** Collect it up front through a `PairingFieldType::Password` schema field, or surface `AwaitingUserInput(InputType::Password)` mid-flow if the device only challenges on connect. Persist it as a `Secret`-visibility field.

`build_config_from_pairing()` merges the pairing result into the persisted vendor config. If your auth needs a credential shape the existing `InputType` can't express, that's the extension point — note it rather than working around it.

## Testing

A backend is fully testable with no hardware. The pattern is a **fake wire server** plus a **connection seam** that points your client at it:

- LG's [`test_server.rs`](../../crates/tisvlay-lg/src) `FakeSsapServer` (`#[cfg(test)] pub(crate)`) speaks the real protocol against `127.0.0.1:0`, with hooks to script registration outcomes, push subscription updates, send raw frames, and drop the connection for fault injection. See [docs/lg/testing.md](../lg/testing.md).
- Epson's `FakeEpsonServer` mirrors the shape for a poll-only transport: a `tokio` TCP responder that performs the `ESC/VP.net` handshake and answers scripted `NAME=VALUE:` / `:` replies, with hooks to set the steady-state codes, **script a warmup/cooldown progression** that advances per `PWR?` read, and drop the connection. No push hook (there's no push channel to fake) — the test polls the same way the daemon does. See [docs/epson/testing.md](../epson/testing.md). Mirror whichever is closer to your transport.
- Layer the tests: pure protocol-codec tests (table-driven encode/decode) → config-parse tests → client integration against the fake server → backend-trait tests → fault injection (auth failure, timeout, mid-transition busy, connection drop). Push the *grammar* into a pure, I/O-free codec module so it is exhaustively table-testable without a socket (Epson's `escvp21.rs` is the model); the fake server then only has to exercise the I/O seams, not re-test the parser.

The workspace test crates compose underneath ([docs/testing.md](../testing.md)): `tisvlay-test` (OS-environment fakes — private D-Bus, fake logind), `tisvlay-mock` (the in-memory backend, for daemon tests), and `tisvlay-harness` (`TestDaemon` / `TestSession` binary-lifecycle fixtures). Your vendor crate owns its own fake server and protocol tests; the daemon-level integration tests use the mock, not your real backend.

## Registering the backend

Three steps, all in `tisvlay-backends`:

1. Add your crate as a dependency in [`crates/tisvlay-backends/Cargo.toml`](../../crates/tisvlay-backends/Cargo.toml).
2. Add a `register_<vendor>()` function in [`crates/tisvlay-backends/src/lib.rs`](../../crates/tisvlay-backends/src/lib.rs) — clone `register_lg`. If your discovery is feature-gated, carry the same `#[cfg_attr(not(feature = "discovery"), expect(unused_variables, …))]` attribute so clippy stays clean when the feature is off; if you have no discovery, pass `discover_fn = None`.
3. Call it from `create_registry()` (and `create_test_registry()` if multi-vendor lookup tests need it).

No other crate changes. See [docs/design.md](../design.md) § Dependency Graph for why this is the only touch-point.

## Vendor-specific docs

Create a `docs/<vendor>/` directory mirroring [docs/lg/](../lg): a `protocol.md` (the wire protocol, command vocabulary, quirks — the reference), a `firmware-quirks.md` (a Symptom → Root Cause → Fix → Regression-Coverage bug inventory, grown as field testing surfaces issues), and a `testing.md` (your fake-server infrastructure). Add the protocol and quirks docs to the Reference Documentation list in [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) once they carry real content.

## Decision rationale

Why the abstraction is shaped this way — the reasoning [docs/design.md](../design.md) states as fact:

### Why vendor config is an opaque `toml::Value`, not a core-known enum
The core has zero vendor knowledge by construction: only your crate can deserialize `[tv.<vendor>]`. A core-side config enum would re-couple every vendor to the core and force a core change per vendor. The cost — a runtime deserialize instead of a compile-time type — buys the "no daemon/CLI/GUI change to add a vendor" property.

### Why schema-driven field visibility, not per-field D-Bus methods
One `config_schema()` row projects to the wizard, INFO pane, and config display automatically. The alternative — a D-Bus method, dispatch arm, and GUI widget per field — is N edits across the stack for what should be one declaration.

### Why `discover()` returns `Vec`, not `Stream`
The vendor trait stays a simple bounded scan; the streaming-discovery machinery (incremental wizard results) lives one layer up in the daemon, so a vendor whose discovery is a single cloud call or broadcast doesn't carry streaming complexity. See [docs/discovery.md](../discovery.md).

### Why every `TvConnection` method defaults to `NotSupported`
Backends are heterogeneous — a projector has no volume, a soundbar has no screen. Defaulting to `NotSupported` lets a backend implement only what it has, and callers gate on the `Capabilities` bits rather than probing.

### Why per-backend `verify_retry_config()`
The reliable `screen_on` / `screen_off` dispatch verifies the transition landed and retries; the right delay and attempt count depend on transport latency (a sub-100 ms WebSocket push vs a poll-only device). Making it a backend knob keeps the reliability machinery transport-agnostic.

### Why a poll-only backend still satisfies the reliability contract
The daemon's verify loop waits on a `watch::Sender<PowerState>` fed by *some* cached-state writer. A push backend feeds it from its subscription; a poll-only backend feeds the same channel from a periodic poll task. The reliability machinery never learns which it is.

### Emulating a capability the device lacks
When a device is missing a discrete capability the daemon expects, a backend can sometimes emulate it with a little server-side state rather than declaring `NotSupported`. Example: a device with volume control but no audio-mute can emulate mute by driving volume to 0 and recalling the prior level on unmute (treating a volume change made while muted as the new recall target). This trades a clean device primitive for daemon-held state that resets on restart — worth it when the emulation is convincing and the capability matters; skip it when the device genuinely can't express the concept.

## Related docs

- [docs/design.md](../design.md) — the trait/contract/D-Bus/config reference, and the power & screen state machine.
- [docs/discovery.md](../discovery.md) — the discovery trait boundary and transport-family bits.
- [docs/testing.md](../testing.md) — the workspace testing framework and harness layers.
- [docs/lg/](../lg) — the worked real-world vendor: `webos.md` (protocol), `firmware-quirks.md` (bug inventory), `testing.md` (`FakeSsapServer`).
- [docs/epson/](../epson) — the second worked vendor: a poll-only, ESC/VP21-over-TCP projector backend.
