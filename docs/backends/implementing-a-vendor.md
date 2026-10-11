# Implementing a vendor backend

A *vendor backend* teaches `tisvlay` how to control one family of devices (LG WebOS TVs, Epson projectors, …). It is one crate that implements four traits from `tisvlay-core` plus one registration entry in the daemon — no CLI, GUI, session, or tray changes. This guide is the *how-to*; the authoritative *contract* is [`crates/tisvlay-core/src/traits.rs`](../../crates/tisvlay-core/src/traits.rs) itself — read the trait definitions alongside this guide, that file is the source of truth for every signature named below. It teaches by pointing at the living reference backends rather than pasting code that would drift: [`tisvlay-mock`](../../crates/tisvlay-mock) is the minimal template you copy from, [`tisvlay-lg`](../../crates/tisvlay-lg) is the real-world network-protocol example, and [`tisvlay-cec`](../../crates/tisvlay-cec) is the non-network, reachable-in-standby example.

## Architecture

The core is vendor- and transport-agnostic. It knows four traits and nothing about how any device actually talks:

- **`TvBackend`** — static metadata: `vendor_id()`, `vendor_name()`, `capabilities()`, and the reliable-dispatch tuning knob `verify_retry_config()`. One trait object per vendor, consulted by the daemon for capability gating.
- **`TvConnector`** — the factory and lifecycle owner: `transport()`, `survives_source_ip_change()`, `reachable_in_standby()`, `connect()`, `wake()` / `wake_and_connect()` / `supports_wake()`, `start_pairing()` / `build_config_from_pairing()`, and the `config_schema()` that drives every UI form. Most of the backend surface lives here.
- **`TvConnection`** — a live, per-device control handle: power, screen, volume, mute, input, backlight, and an optional `subscribe_state()` push channel. **Every feature method defaults to `Err(NotSupported)`** — a backend overrides only what its hardware does.
- **`TvDiscovery`** — an optional, bounded `discover(timeout) -> Vec<DiscoveredTv>` scan. Backends with no discovery protocol simply don't implement it (and omit `Capabilities::DISCOVERY`).

The single composition point is the daemon: [`tisvlay-daemon`](../../crates/tisvlay-daemon) is the *only* crate that imports vendor crates (one `VendorReg` entry per vendor in its `run()`). Everything else (CLI, GUI, session, tray) stays vendor-blind — clients learn the vendor catalog and each vendor's pairing schema from the daemon's `manager/vendors` queryables over zenoh. The whole vendor config travels as an opaque [`VendorConfig`](../../crates/tisvlay-core/src/vendor_config.rs); only your crate deserializes it into your config type, through `VendorConfig::parse`. The daemon reads it only generically: the schema-driven display rows and the shared `mac`/`wake_id` keys (Wake-on-LAN and the pair-time MAC fill).

## Crate structure

One trait impl per same-named file — the layout `tisvlay-mock` (minimal) and `tisvlay-lg` (real-world) both follow:

| Concern | file | `tisvlay-mock` | `tisvlay-lg` |
|---------|------|----------------|--------------|
| `TvBackend` impl | `backend.rs` | metadata only | metadata only |
| `TvConnector` impl | `connector.rs` | ✓ | ✓ |
| `TvConnection` impl | `connection.rs` | ✓ | ✓ |
| `TvDiscovery` impl | `discovery.rs` | ✓ | ✓ (feature-gated) |
| Vendor config struct | `config.rs` | ✓ | ✓ |
| Wire layer (sized to the protocol) | — | none (in-memory) | `client.rs`, `transport.rs`, `protocol.rs`, `endpoints.rs` |
| `lib.rs` | re-exports + `VENDOR_ID` + factory fns | ✓ | ✓ |

**The invariant: one trait impl per same-named file (`backend` / `connector` / `connection` / `discovery`) plus `config.rs`.** Past that, the wire layer scales to your protocol — the mock needs none (its "wire" is an in-memory state machine), while LG's SSAP-over-WebSocket spreads across `client.rs` (the high-level command API), `transport.rs` (the raw WebSocket + request/response multiplexing), `protocol.rs` (message types), and `endpoints.rs` (constants). A tiny backend *could* fuse the trait impls into fewer files, but both references split for clarity — prefer it; the daemon can't tell the difference, so what matters is that each file stays readable. Discovery rides the shared [`tisvlay-ssdp`](../../crates/tisvlay-ssdp/) crate (SSDP M-SEARCH) rather than a per-vendor reimplementation — see LG's `discovery.rs`. Every crate exposes its `VENDOR_ID` / `VENDOR_NAME` constants and a constructor from `lib.rs` (see [`tisvlay-mock/src/lib.rs`](../../crates/tisvlay-mock/src/lib.rs) `new_production_parts()` / `new_test_parts()`).

### Test code comes in three flavors

- **Inline unit tests** — a `#[cfg(test)] mod tests` at the bottom of each file, covering that file's concern; compiled only for your crate's `cargo test`.
- **A `#[cfg(test)] pub(crate)` fake-server file** — `test_server.rs` (LG's `FakeSsapServer`): a scriptable stand-in for the real device that drives your client through its real code paths with no hardware. The inline integration tests call it; build one.
- **Mock-only test-driving scaffolding** — the mock's `handle.rs` / `state.rs` / `pairing.rs` are an always-compiled control surface (`MockHandle`) over an injectable state store, so *external* test crates can drive the mock (publish discoverable TVs, force pairing outcomes, inject state). A real backend has no equivalent — it talks to hardware. **When cloning mock as a skeleton, copy `backend.rs` / `connector.rs` / `connection.rs` / `config.rs` / `discovery.rs`; skip `handle.rs` / `state.rs` / `pairing.rs`.**

## Implementing the four traits

Work in this order; lean on the mock as a skeleton and the LG impl when you need the real-network version.

1. **`TvBackend`** — return your `vendor_id` / `vendor_name` and OR together the `Capabilities` bits your hardware supports (next section). Override `verify_retry_config()` only if your transport's latency differs materially from the LG-tuned default. See `MockBackend` / `LgBackend`.

2. **`TvConnector`** — the heart of the backend.
   - `transport()` returns `Transport::{Network, Serial, Usb, Cec}`; the daemon scopes carrier/route events to `Network` backends via `is_network_dependent()`.
   - `survives_source_ip_change()` declares whether your protocol survives the client's source IP changing on a NIC failover — `false` for a persistent socket (WebSocket, raw TCP), `true` for a stateless request-per-command transport.
   - `reachable_in_standby()` declares whether the transport still reaches the device while it is *powered off* — `false` by default (a network TV drops its socket in standby, so `Connected` tracks power), `true` for a bus that stays live in standby (CEC: the HDMI wire answers even when the panel is dark). Returning `true` decouples `Connected` from power — the daemon then accepts a standby device in `connect()`, keeps the connection across `power_off` (only the `PowerState` moves to standby), and **wakes in place** on the existing connection instead of tearing it down (see the `TvConnection::power_on` note below). See `tisvlay-cec` and [docs/cec/protocol.md](../cec/protocol.md) for the worked model.
   - `connect()` makes one fail-fast attempt and returns a `Box<dyn TvConnection>`. `wake_and_connect()` owns *all* retry/backoff/probe logic for the wake path — the daemon calls it and treats it as atomic. `supports_wake()` gates Wake-on-LAN per device (e.g. on whether a MAC is configured). Reuse the generic [`tisvlay_core::wol`](../../crates/tisvlay-core/src/wol.rs) helpers rather than re-inlining the send loop — `wake_with_retry(mac, max_attempts)` for the initial send (it backs off while the local NIC recovers after resume) and `wake_best_effort(mac)` for a single resend inside a connect-retry loop.
   - `start_pairing()` / `build_config_from_pairing()` drive the pairing state machine (see § Pairing and authentication).
   - `config_schema()` declares every vendor-config field once; the framework projects it into the pairing wizard, the INFO pane, and config display (see § Capabilities, config schema, and field visibility).

3. **`TvConnection`** — override the feature methods your device supports (`power_off`, `get_power_state`, `screen_on` / `screen_off` / `is_screen_on`, `set_volume` / `set_mute`, `switch_input` / `list_inputs`, `set_backlight`, …). Leave the rest at their `NotSupported` defaults. `get_state()` returns a full [`TvState`](../../crates/tisvlay-core/src/state.rs) snapshot. If your protocol pushes asynchronous updates, implement `subscribe_state()` to return an `mpsc` receiver of cumulative `TvState` snapshots (LG does); if it is poll-only, leave it unimplemented and the daemon falls back to periodic polling. Map your device's raw power substates onto the `PowerState` enum ([`crates/tisvlay-core/src/state.rs`](../../crates/tisvlay-core/src/state.rs)) — including the transient `WarmingUp` / `CoolingDown` for warmup/cooldown-gated devices. **`power_on` (wake-in-place)** defaults to `NotSupported` and matters only for a `reachable_in_standby` backend: implement it to wake the device on *this* live connection (CEC sends `Image View On`), so the daemon can wake a standby-but-connected device without tearing the connection down and flapping `Connected`. A backend whose connection dies in standby never reaches this — the daemon wakes it via the connector's `wake_and_connect()` instead. **`get_link_mac` (device-reported MAC)** defaults to `None` and matters only if your config's `mac` field (`tisvlay_core::MAC_CONFIG_KEY`) drives wake: if the device can report the MAC of the interface the connection reached it through, implement it, and the daemon reads it once on the pairing-verification dial to fill a blank `mac` (and to flag a configured one that disagrees). It's the MAC source that works for a TV on another subnet, where ARP only sees the gateway. Keep it best-effort and cheap: the daemon bounds it with a short timeout before publishing the pairing's `complete`. LG joins two `connectionmanager` reads on the dialed address; CEC has no such query and keeps the default.

4. **`TvDiscovery`** (optional) — implement `discover(timeout) -> Vec<DiscoveredTv>` only if you have a discovery protocol, and feature-gate it (LG gates SSDP behind a `discovery` feature). Return SSDP-shaped `DiscoveredTv` entries so the daemon's post-discovery MAC enrichment and `autofill: "discovery.<key>"` wizard pre-fill apply. See [docs/discovery.md](../discovery.md) § Trait boundary for why this is a bounded `Vec`, not a stream.

## Capabilities, config schema, and field visibility

`Capabilities` ([`crates/tisvlay-core/src/capabilities.rs`](../../crates/tisvlay-core/src/capabilities.rs)) is a bitflag set the CLI/GUI use to gate affordances: `POWER`, `SCREEN`, `VOLUME`, `MUTE`, `INPUT`, `BACKLIGHT`, `WOL`, `DISCOVERY`, plus the transport-family bits `IPV4` / `IPV6` (deliberately *not* in `STOCK` — opt in per family you have actually verified). Declare a bit only when the matching `TvConnection` method really works; clients trust the bits.

Your vendor config is a serde struct (your `config.rs`) parsed from the opaque `VendorConfig`, and `build_config_from_pairing` returns one through `VendorConfig::from_typed`. Type a credential field as `tisvlay_core::Secret` so it can't leak through `Debug`. `config_schema()` returns a `Vec<PairingField>` describing each field, and each field carries a `FieldVisibility`:

- **`UserInput`** — the user types it; shown in the pairing wizard and the INFO pane, and editable.
- **`Diagnostic`** — backend-discovered or read-only; shown in the INFO pane only.
- **`Secret`** — credentials; filtered from every display and never sent to the GUI.

Declare the schema once and the framework projects it everywhere — adding an editable field is one schema row, with no daemon rpc, CLI, or GUI rendering changes. Order the schema so the most informative `UserInput` field comes first; it becomes the backend's "primary descriptor" (`TvConnector::primary_descriptor`, a schema-driven default) — used in log lines, and, when it parses as an IP, as the address the daemon route-monitors for carrier/route scoping ([docs/watchers/network.md](../watchers/network.md)).

## Pairing and authentication

The same `PairingState` machine ([`crates/tisvlay-core/src/pairing.rs`](../../crates/tisvlay-core/src/pairing.rs): `AwaitingTvPrompt`, `AwaitingUserInput { input_type }`, `Complete { credentials }`, `Failed`) spans a wide range of auth models. The three references show the spectrum:

- **No real auth (trivial).** The mock auto-accepts via a config shorthand. A backend whose transport is unauthenticated (e.g. raw TCP) validates connectivity in `start_pairing()` and emits `Complete` with empty credentials immediately — the simplest end of the machine.
- **Interactive accept-on-device with a returned credential.** LG races a `tokio::select!` between the TV's user-approval handshake and a cancel command, captures the returned client key, and stores it via `build_config_from_pairing()`. Uses `AwaitingTvPrompt`.
- **A user-supplied secret (password / PIN).** Collect it up front through a `PairingFieldType::Password` schema field, or surface `AwaitingUserInput(InputType::Password)` mid-flow if the device only challenges on connect. Persist it as a `Secret`-visibility field.

`build_config_from_pairing()` merges the pairing result into the persisted vendor config. If your auth needs a credential shape the existing `InputType` can't express, that's the extension point — note it rather than working around it.

## Testing

A backend is fully testable with no hardware. The pattern is a **fake wire server** plus a **connection seam** that points your client at it:

- LG's [`test_server.rs`](../../crates/tisvlay-lg/src) `FakeSsapServer` (`#[cfg(test)] pub(crate)`) speaks the real protocol against `127.0.0.1:0`, with hooks to script registration outcomes, push subscription updates, send raw frames, and drop the connection for fault injection. Mirror this shape for your transport. See [docs/lg/testing.md](../lg/testing.md).
- Layer the tests: pure protocol-codec tests (table-driven encode/decode) → config-parse tests → client integration against the fake server → backend-trait tests → fault injection (auth failure, timeout, mid-transition busy, connection drop).

The workspace test layers compose underneath ([docs/testing.md](../testing.md)): `tisvlay-test` (OS-environment fakes — private D-Bus, fake logind/ScreenSaver), `tisvlay-mock` (the in-memory backend, always registered in the daemon), and wire-level integration tests in each binary crate's `tests/` that boot real daemon/session stacks on temp sockets. Your vendor crate owns its own fake server and protocol tests; the daemon-level integration tests use the mock, not your real backend.

## Registering the backend

Two steps, both in `tisvlay-daemon`:

1. Add your crate as a dependency in [`crates/tisvlay-daemon/Cargo.toml`](../../crates/tisvlay-daemon/Cargo.toml).
2. Add a `VendorReg` entry to the `vendors` list in `run()` ([`crates/tisvlay-daemon/src/lib.rs`](../../crates/tisvlay-daemon/src/lib.rs)) — clone the LG entry:

   ```rust
   VendorReg::new("acme", tisvlay_acme::VENDOR_NAME, Arc::new(AcmeConnector::new()))
       .with_capabilities(caps(&tisvlay_acme::AcmeBackend))
       .with_discovery(Arc::new(AcmeDiscovery::new()))
   ```

   `with_discovery` takes any `TvDiscovery` implementor (a blanket impl adapts it); skip the call if you have no discovery.

No other crate changes — clients pick up the new vendor from the daemon's catalog queryables at runtime.

## Vendor-specific docs

Create a `docs/<vendor>/` directory mirroring [docs/lg/](../lg): a `protocol.md` (the wire protocol, command vocabulary, quirks — the reference), a `firmware-quirks.md` (a Symptom → Root Cause → Fix → Regression-Coverage bug inventory, grown as field testing surfaces issues), and a `testing.md` (your fake-server infrastructure). Add the protocol and quirks docs to the Reference Documentation list in [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) once they carry real content.

## Decision rationale

Why the abstraction is shaped this way:

### Why vendor config is an opaque `VendorConfig`, not a core-known enum
The core has zero vendor knowledge by construction: only your crate can deserialize the vendor config into its typed form. A core-side config enum would re-couple every vendor to the core and force a core change per vendor. The cost — a runtime deserialize instead of a compile-time type — buys the "no daemon/CLI/GUI change to add a vendor" property. The wrapper around the TOML table exists for one reason: it may hold a credential, so its `Debug` prints key names only, and a world or daemon holder can't log the key by accident.

### Why schema-driven field visibility, not per-field rpc plumbing
One `config_schema()` row projects to the wizard, INFO pane, and config display automatically. The alternative — an rpc verb, dispatch arm, and GUI widget per field — is N edits across the stack for what should be one declaration.

### Why `discover()` returns `Vec`, not `Stream`
The vendor trait stays a simple bounded scan; the streaming-discovery machinery (incremental wizard results) lives one layer up in the daemon, so a vendor whose discovery is a single cloud call or broadcast doesn't carry streaming complexity. See [docs/discovery.md](../discovery.md).

### Why every `TvConnection` method defaults to `NotSupported`
Backends are heterogeneous — a projector has no volume, a soundbar has no screen. Defaulting to `NotSupported` lets a backend implement only what it has, and callers gate on the `Capabilities` bits rather than probing.

### Why `reachable_in_standby` decouples `Connected` from power
Most transports die when the device powers off, so "connected" and "on" move together, and the default `false` collapses them into one axis — a backend author never has to think about it. A bus that stays live in standby (CEC) breaks that coupling: the device is reachable-but-off, so `Connected` and `PowerState` become independent axes. Making it a per-connector predicate keeps the daemon's power flow uniform — it always asks "is this transport reachable in standby?" rather than special-casing one vendor — and pairs with `TvConnection::power_on` so a wake reuses the live connection instead of a disruptive reconnect that would flap `Connected`. This is why a CEC button-off and remote-off both report the honest `Connected: true, Power: standby`, while a network TV still reads disconnected when off.

### Why per-backend `verify_retry_config()`
The reliable `screen_on` / `screen_off` dispatch verifies the transition landed and retries; the right delay and attempt count depend on transport latency (a sub-100 ms WebSocket push vs a poll-only device). Making it a backend knob keeps the reliability machinery transport-agnostic.

### Why a poll-only backend still satisfies the reliability contract
The daemon's verify loop waits on a `watch::Sender<PowerState>` fed by *some* cached-state writer. A push backend feeds it from its subscription; a poll-only backend feeds the same channel from a periodic poll task. The reliability machinery never learns which it is.

### Emulating a capability the device lacks
When a device is missing a discrete capability the daemon expects, a backend can sometimes emulate it with a little server-side state rather than declaring `NotSupported`. Example: a device with volume control but no audio-mute can emulate mute by driving volume to 0 and recalling the prior level on unmute (treating a volume change made while muted as the new recall target). This trades a clean device primitive for daemon-held state that resets on restart — worth it when the emulation is convincing and the capability matters; skip it when the device genuinely can't express the concept.

A second worked example — **CEC's screen-via-power** — emulates `SCREEN` with the `POWER` primitive (HDMI-CEC has no discrete screen-blank), gated behind a per-device `emulate_screen_via_power` config (default on). Its lesson is *where* the emulation lives: the mapping is done at the **daemon dispatch** (a config-aware `TvConnector::emulate_screen_via_power()` predicate the daemon reads to advertise `SCREEN` and route `screen_off`/`screen_on` to `power_off`/`power_on`), **not** inside the connection's `screen_off`/`screen_on`. Doing it at the daemon reuses the correct power path (keep the connection across standby, synthesize the standby cache the TV won't re-broadcast, wake in place); doing it in the connection would bypass that and leave the cached power state stale. Rule of thumb: emulate at the layer that owns the primitive you're mapping *to*.

## Related docs

- [docs/architecture.md](../architecture.md) — the current architecture reference (zenoh key space, world/reactor model, topology).
- [docs/discovery.md](../discovery.md) — the discovery trait boundary and transport-family bits.
- [docs/testing.md](../testing.md) — the workspace testing framework and layers.
- [docs/lg/](../lg) — the worked real-world vendor: `webos.md` (protocol), `firmware-quirks.md` (bug inventory), `testing.md` (`FakeSsapServer`).
- [docs/cec/](../cec) — the non-network worked vendor: HDMI-CEC over the Linux CEC subsystem, and the canonical `reachable_in_standby` (Connected ≠ power) model.
- [docs/design.md](../design.md) — the retired served-D-Bus architecture (≤ v0.2.0); historical background for the trait rationale.
