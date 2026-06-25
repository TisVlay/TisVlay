# CEC backend (`tisvlay-cec`)

A new vendor backend that controls TVs over HDMI-CEC through the Linux CEC subsystem (`/dev/cecN`), using Valve's [`linux-cec`](https://gitlab.steamos.cloud/holo/linux-cec/) library. It is the first non-network, non-vendor-protocol backend — `Transport::Cec` — and the first whose capability set is well below `STOCK`, which (see § Cross-stack reality) is what makes it valuable *and* what surfaces several dormant assumptions in the rest of the stack.

> Status: reviewed and settled — design / impl-correctness / maintainability passes folded in; the capability-gating scope is decided (gate properly, as a prerequisite slice). Ready to implement.

## Goal & v1 scope

Ship a CEC backend whose **v1 surface is power + active-source input**: turn the TV on/off, read its power state, and switch the TV to a given HDMI input (primarily "switch to the input our adapter is plugged into"). Volume, mute, backlight, and OLED screen-blank are explicitly **out of v1** (see § Capability mapping). The backend is built and tested **without a real TV-CEC link**, because no such link exists on the development machine today (see § Hardware reality) — real-hardware validation is a later, parallel track gated on a USB CEC adapter.

## Locked decisions

1. **Integration = the `linux-cec` library, statically linked** (its `async` / tokio `AsyncDevice` wrapper), as a git-pinned or vendored dependency (not on crates.io). It fits our architecture exactly — the backend owns its `/dev/cecN` handle, and `tisvlayd` is the natural single owner of the CEC adapter, so we don't need `cecd`'s multi-app arbitration. Standing up `cecd` as a second system daemon was rejected: its own wake/suspend-sync overlaps tisvlay's power management, its D-Bus surface is coarser, and it isn't packaged for our targets. The raw `linux-cec-sys` ioctl layer (BSD-3) was rejected as reinventing the message/operand codec.
   - **Pin away the default features.** `linux-cec`'s defaults are `["async", "cectool", "tracing"]`; `cectool` pulls `clap` and builds a CLI binary we don't want. Depend with `default-features = false, features = ["async"]` (add `"tracing"` if we want its spans).
   - **License is a CI gate, not a footnote.** tisvlay is Apache-2.0; `linux-cec` is LGPL-2.1-or-later. The repo enforces an SPDX allow-list via `cargo-deny` (`just deny`, see [docs/licensing.md](../licensing.md)) — **LGPL not on the exception list fails the build.** So the first commit must add a `linux-cec` per-crate exception to **both** `packaging/licensing/deny.toml` and `packaging/licensing/about.toml`, plus an entry in `docs/licensing.md` documenting the LGPL §6 static-relink obligation (heavier than the repo's existing MPL file-level precedent). Low practical friction for an open-source project (source is available to relink against), but revisit if tisvlay ever ships a closed binary — `linux-cec-sys` alone is BSD-3 as the escape hatch.

2. **v1 capabilities = `POWER | INPUT`.** No `VOLUME`, `MUTE`, `BACKLIGHT`, or `SCREEN`. Crucially, **declare neither `IPV4` nor `IPV6`** — beyond "not a network transport," this is load-bearing: the daemon's manual-entry enrichment parses the primary descriptor as an IP and ARP-probes it, gated on those bits (`tisvlayd/src/dbus/manager.rs`), so omitting both guarantees a `/dev/cecN` descriptor is never mis-enriched as an IP/MAC. Relative volume (the only volume CEC portably offers) is a deferred, cross-cutting capability-model extension — its own plan.

3. **Software-first, fake/virtual CEC for tests.** Unit tests run against a fakeable seam at the `linux-cec` boundary; real hardware (the incoming USB CEC adapter) and its NixOS enablement are a separate, later track.

## Hardware reality (development machine)

Recorded so the "software-first" stance is legible and we don't re-investigate:

- The TV is driven from `card1-DP-3` via a UGREEN active DP→HDMI adapter on the **NVIDIA open kernel module** (595.45.04). No nvidia module references any `cec` / `drm_dp_cec` symbol — **nvidia-drm does not implement DP-AUX CEC tunneling** (amdgpu and i915 do; nvidia does not). There is no `/dev/cec*`, and software cannot fix this.
- The i9-14900K **does** contain an iGPU (UHD 770), but it is **disabled in UEFI** (PCI `00:02.0` absent) and is not a clean CEC path anyway: the TV's video must stay on nvidia for gaming, and CEC can't ride the nvidia cable, so the iGPU could only contribute CEC via a second cable to a spare TV input (and this ASUS ProArt Z790-Creator board exposes the iGPU DP only over Thunderbolt USB-C). `i915` (which *does* tunnel DP-CEC) is in the kernel build, making the iGPU the only no-kernel-rebuild route to a real `/dev/cec` for development.
- The kernel **`cec` core module is available**. `vivid` (virtual CEC) and `pulse8-cec` (likely driver for the incoming USB adapter) are **not** in the current NixOS kernel build — both are enablement steps.
- **Production CEC path = the USB CEC adapter**, which joins the TV's CEC bus over USB↔HDMI independently of which GPU draws video.

## Capability mapping (what CEC can and can't do)

| tisvlay capability | CEC reality | v1 |
|---|---|---|
| `POWER` on/off | `ImageViewOn` (on) / `Standby` (off) — reliable | ✅ |
| power **state** | `GiveDevicePowerStatus` → `ReportPowerStatus`, whose operand is `PowerStatus::{On, Standby, ToOn, ToStandby}` (`linux-cec` `operand.rs`). Mapping `ToOn`→`WarmingUp` and `ToStandby`→`CoolingDown` is a **deliberate approximation, not a clean 1:1** — CEC "in transition" is not a projector light-source warmup; the alternative is to fold both into `Unknown`. Decide and justify in code | ✅ |
| `INPUT` switch | `ActiveSource` / `SetStreamPath` to a physical address — reliable for "switch the TV to this HDMI input"; rich enumeration / current-input readback is **not** portable | ✅ (switch); readback best-effort |
| `VOLUME` | No absolute set. Only **relative** `UserControlPressed{VolumeUp/VolumeDown}`. Absolute readback via `ReportAudioStatus` is device-dependent (an AVR/soundbar feature). Needs a new relative-volume primitive | ❌ deferred |
| `MUTE` | `UserControlPressed{Mute}` is a **toggle**, not set-state; readback device-dependent | ❌ deferred |
| `BACKLIGHT`, `SCREEN`-blank | Not CEC concepts | ❌ omit |

Two model notes that shape the impl:

- **Power-on is the connector's `wake()` path** (send `ImageViewOn` + `ActiveSource`), with a `ReportPowerStatus == On` verify. `supports_wake()` returns `true` **when the device path is configured** (not unconditionally — the GUI shows the wake button based on it, `tvs_pane.slint`). `power_off()` sends `Standby`. Set `POWER` but **not** `WOL` — CEC wake is not a magic packet, and the daemon routes `PowerOn` through `supports_wake()`, never through the `WOL` bit (`tv_manager.rs`), so POWER-without-WOL powers on correctly.
- **`subscribe_state()` is `None` in v1 (poll-based `get_state()`).** Consequence to own: with no push channel, `reconcile_screen` fires only on connect and `state_poll_loop` refreshes only every ~20 s (`tv_manager.rs`), so a TV powered off **by its own remote** can hold stale `Connected`/`PowerState` in D-Bus for up to that interval, and the DPMS-mirror won't re-assert until the next poll. The deferred `rx`-driven subscription (CEC TVs *do* broadcast unsolicited `Standby`/`ActiveSource`/`ReportPowerStatus`) is the remedy that restores parity with push backends — list it as such, not a mere nicety.

## Cross-stack reality: CEC is the first sub-`STOCK` backend

LG and mock both declare nearly-full `STOCK`, so the stack was built **capability-blind** in two places that CEC is the first to expose. These are not optional polish — they determine whether a CEC TV is usable and correct.

### A. Clients don't gate controls on capabilities — they gate on `connected`

The "adding a vendor touches only the new crate + `tisvlay-backends`" promise from [the vendor guide](../backends/implementing-a-vendor.md) does **not** hold for a sub-`STOCK` backend. Verified:

- The per-vendor `Capabilities` exists on the wire (`tisvlayd/src/dbus/tv.rs` `Tv1.Capabilities`) but is **dropped** from the `TvInfo` snapshot every client consumes (`tisvlay-core/src/dbus_client.rs` has no `capabilities` field).
- GUI per-TV PICTURE/AUDIO groups gate on `enabled: root.tv.connected`, not capabilities (`tisvlay-gui/ui/components/tvs_pane.slint`). The GUI fetches caps but uses only `IPV4`/`IPV6`, for the wizard (`state_watch.rs`).
- Tray pushes Backlight/Screen/Volume/Mute items unconditionally for any connected TV (`tisvlay-tray/src/tray.rs`); only the Input submenu is guarded.
- CLI prints `Volume:`/`Backlight:` whenever the TV is on (`tisvlay-cli/src/main.rs`), and the daemon coerces absent `Option<u8>` volume/backlight to `0` on the wire (`tisvlayd/src/dbus/tv.rs`), so a CEC TV surfaces `Volume: 0` rather than "absent."

Net: a connected power+input CEC TV renders **live, enabled Volume/Screen/Backlight controls that silently no-op** (`set_volume` → `NotSupported` → swallowed `warn!`) and prints `Volume: 0`. This is a pre-existing latent gap (the projector backend would hit it too); CEC is just the first to ship it to a user's eyes. We fix it — § Prerequisite (decided): capability gating.

### B. `connect()` / `is_connected()` must encode "TV reachable for control," not "device open"

The daemon was built around a network TV: *unreachable when off, reachable when on.* CEC inverts this — the `/dev/cecN` handle opens even in standby. The landmine: `TvManager::power_on` chooses direct-connect vs. wake by **whether `connect()` succeeds** (`tv_manager.rs`). If CEC `connect()` always succeeds (device opens regardless of TV power), `power_on` always takes the direct branch and **never sends `ImageViewOn` — the TV never turns on.** Therefore:

- CEC's `connect()` must probe `GiveDevicePowerStatus` and return `Err` when the TV is in `Standby`, so the probe falls through to `wake_and_connect()` → `ImageViewOn`.
- `is_connected()` must reflect TV-reachable-for-control, not merely device-open, or the reconcile/verify state machine treats a standby TV as live.
- Mind the timeout interaction: `power_on` wraps the probe in a ~2 s `DIRECT_CONNECT_PROBE_TIMEOUT` (`tv_manager.rs`), and `linux-cec` per-message timeouts top out near 1 s — reconcile these in the connect-probe design and in `verify_retry_config()` (the trait explicitly anticipates a USB-CEC override).

## Crate design — `crates/tisvlay-cec/`

Follows the one-trait-impl-per-file convention from [docs/backends/implementing-a-vendor.md](../backends/implementing-a-vendor.md), cloning `tisvlay-mock` as the skeleton (skip its `handle.rs`/`state.rs`/`pairing.rs` test scaffolding).

- **`backend.rs`** — `CecBackend`: `vendor_id()` = `"cec"`, `vendor_name()` = `"HDMI CEC"`, `capabilities()` = `POWER | INPUT` (no `IPV4`/`IPV6`). Override `verify_retry_config()` for CEC bus latency.
- **`config.rs`** — `CecVendorConfig`: the `/dev/cecN` device path (UserInput, primary descriptor); discovered physical address (Diagnostic); optional OSD name we advertise. **Identity hook (deferred, net-new — not a free dovetail):** `linux-cec`'s `get_connector_info()` returns `ConnectorInfo::DrmConnector { card_no, connector_id }` — numeric DRM card + KMS connector id, **not** the sysfs connector name (`HDMI-A-1`) that the daemon's `desktop_probe.rs`/`dpms.rs`/`Tv1.Desktop.Connector` are keyed on, and possibly `ConnectorInfo::None` on a USB adapter. Correlating the two needs a new resolution step (walk `/sys/class/drm/card{card_no}-*`, match the KMS id). Treat as future work.
- **`connector.rs`** — `CecConnector`: `transport()` = `Transport::Cec`; `survives_source_ip_change()` = `false`. `connect()` opens the device, claims a `playback` logical address, **probes power and fails when the TV is in standby** (§ Cross-stack B). `start_pairing()` has **no real auth** — validate the device opens and probe the bus for the TV's physical address, then emit `Complete` with empty credentials immediately (the daemon then calls `build_config_from_pairing()`, which for CEC just persists the device path + diagnostics; only LG's impl asserts a `scheme`, irrelevant here). `config_schema()` declares the fields above.
- **`connection.rs`** — `CecConnection`: `power_off()` (`Standby`), `get_power_state()` (map `PowerStatus`), `switch_input()` (`SetStreamPath`/`ActiveSource` to the target physical address). `get_current_input()` is **best-effort = return `Ok(None)` when unreadable, never `Err(NotSupported)`** (the trait returns `Result<Option<String>>`; an error would surface to clients as a failure rather than "unknown"). `get_state()` assembles `TvState` from power + active-source. Everything else stays at the `NotSupported` default.
- **`discovery.rs`** — enumerate `/dev/cecN`, read each adapter's connector/OSD/vendor, return `DiscoveredTv { address = device path, … }`. **Non-optional** (§ Prerequisite): the wizard's manual-entry path is network-shaped, so the device picker is the primary entry path. (Still keep the impl clean enough to feature-gate later if a headless build wants it out.)
- **`link.rs` (the seam)** — a thin internal trait (`CecLink`) wrapping exactly the `linux-cec` calls we use: open, set logical address, tx `Message`, **tx-rx for `GiveDevicePowerStatus`→`ReportPowerStatus`** (first-class, since the connect-probe correctness in § B hinges on mocking it), `wake`/`standby`/`set_active_source` helpers, and `close`. Production impl wraps `AsyncDevice`; tests use a scripted `FakeCecLink`. Signature realities to design around: `AsyncDevice` methods take **`&self`** (good — the seam trait can be `&self`), but `wake(set_active: bool, text_view: bool)` and `standby(target: LogicalAddress)` are **not** zero-arg, and `close(self)` consumes by value. `AsyncDevice` also runs a **dedicated OS thread per device and `join()`s it on `close`/`Drop`** (`async_support.rs`); since the daemon calls `close().await` while holding the per-TV `Mutex<ManagedTv>` (the documented drain layer), `CecLink::close` must **bound/guard that join**, not block on a wedged thread. Named `link.rs`, not `device.rs`, to avoid confusion with `linux-cec`'s own `device.rs`/`async_support.rs`.
- **`lib.rs`** — `VENDOR_ID`/`VENDOR_NAME` constants, re-exports, production/test factory fns.

## Wiring it in (the real touch-points)

The vendor guide's "two crates, three edits" is the *registration* core, but CEC's sub-`STOCK` shape and chardev transport mean the full change set is wider. Honest list:

- **`tisvlay-backends`** — add the dep in its `Cargo.toml`; add `register_cec()` (clone `register_lg`, `discover_fn = None` until discovery lands); call from `create_registry()` (and `create_test_registry()` if needed).
- **Root `Cargo.toml`** — add `crates/tisvlay-cec` to `members` (explicit array, no glob) and a `[workspace.dependencies]` entry so `tisvlay-cec.workspace = true` resolves.
- **Licensing** — `deny.toml` + `about.toml` exception + `docs/licensing.md` entry (§ Locked decision 1).
- **systemd hardening** — § NixOS / packaging.
- **Capability gating in clients** — § Prerequisite (decided): capability gating (the scope-expanding item; lands as step 0).

## Testing strategy

- **Seam-level unit tests** drive `CecConnector`/`CecConnection` through a scripted `FakeCecLink`: power on/off/state transitions (incl. the connect-probe-fails-when-off path), active-source switch, and fault injection (device gone mid-call, bus busy, no TV present, transition power states). **Framing honestly:** this is a *new* idiom for the workspace, justified by the C-FFI boundary — unlike LG's `FakeSsapServer` (a real localhost socket the *production transport* dials) or the mock (an in-memory `TvConnection`), faking at the library wrapper means production `linux-cec` ioctls aren't exercised by these tests. So **pair it with a mock-tier in-memory `TvConnection` fake** at the daemon level to cover the state-forwarder / property-cache path — that's the layer that actually surfaces the `Volume = 0` coercion and the always-reachable state-machine behavior.
- **Layered** like the LG guide: config parse → message/flow mapping → connector pairing/connect → connection power/input → fault injection.
- **`vivid` real-ioctl coverage is deferred, not a v1 test bullet** — `vivid` isn't in the NixOS kernel build, so it would never run in CI without a kernel-config change. Keep it in Risks as "future real-ioctl coverage," with `FakeCecLink` + the mock-tier fake as the v1 paths.
- Daemon-level integration tests continue to use `tisvlay-mock`.

## NixOS / packaging enablement

Kept narrow here (broader story is a separate packaging task), but two layers, both required for the device to be reachable:

- **Filesystem permissions** — `/dev/cecN` is not world-accessible and `tisvlayd` runs as the `tisvlay` user: ship a udev rule / group membership granting access.
- **systemd sandbox** — the daemon runs under `PrivateDevices = true` (`packaging/nix/module.nix` **and** `packaging/systemd/tisvlayd.service`), which gives a minimal private `/dev` with **no `/dev/cec*` at all**, regardless of host udev. Pair it with `DeviceAllow=char-cec rw` (or drop `PrivateDevices`), and verify `linux-cec`'s ioctl set against the `@system-service ~@privileged ~@resources` `SystemCallFilter`. Note the existing sysfs watchers (`dpms.rs`, `desktop_probe.rs`) are unaffected because `/sys` isn't a device node — this constraint is genuinely new for a CEC chardev. Both the Nix module and the shipped `.service` need the change.
- **Kernel modules** — document that real hardware needs `cec` (available) plus the adapter driver (`pulse8-cec` for the USB unit), which the current NixOS kernel build lacks. Instructions, not a hard dependency.
- **Debug tooling** — `v4l-utils` (`cec-ctl`) as a devshell convenience.

## Prerequisite (decided): capability gating

CEC is the first backend that genuinely lacks volume/screen/backlight, and the decision is to **gate properly** — the correct fix, and every future heterogeneous backend (including the projector) benefits. This is a prerequisite slice that lands before/alongside CEC (sequencing step 0):

- Thread `capabilities` into `tisvlay-core`'s `TvInfo` (it already rides the wire as `Tv1.Capabilities`; it's just dropped from the snapshot at `dbus_client.rs`).
- Gate the GUI PICTURE/AUDIO groups on the bits (`tvs_pane.slint` + a `capabilities` field on `TvViewRaw`/the Slint `TvView`), the tray's backlight/screen/volume/mute sections (`tray.rs`), and the CLI's volume/backlight lines (`main.rs`).
- Touches core + GUI + tray + CLI. Independently useful; landed first, it lets CEC ship an honest surface from its first commit.

A second, smaller item rides along — the **pairing-wizard manual-entry step is network-shaped** (labeled "IP", placeholder `192.168.1.100`, autofill seeds only fields named `ip`/`address` — `pairing_wizard.slint`, `schema_form.rs`). **Resolved default:** name the CEC primary-descriptor field `address` (so the existing autofill plumbing works) and make CEC discovery **non-optional** so the device picker is the primary entry path; generalizing the manual-entry "IP" label to the schema's field label is a small, separate follow-up. (Open to overriding if you'd rather keep discovery optional.)

## Risks & deferred work

- **Dependency provenance / LGPL** — pin `linux-cec` to a git rev or vendor it; the `cargo-deny` exception and `docs/licensing.md` entry are step-1 build-blockers (§ Locked decision 1).
- **Weak input readback** — "switch the TV to our input" is the reliable op; current-input/input-list are best-effort (`Ok(None)`).
- **DPMS-mirror may not engage for USB-CEC** — the watcher matches a TV to its *video* connector via EDID, while the CEC TV is reached over `/dev/cecN` on a separate cable; the CEC adapter's own connector info points elsewhere or is `None`. Known v1 limitation pending the identity-correlation work.
- **Real-hardware lag** — validation waits on the USB adapter + NixOS module enablement; fake/mock-tier only until then.
- **Relative volume** — a future cross-cutting capability (`Capabilities` bit + `TvConnection` methods + D-Bus + CLI + GUI). Its own plan.
- **`rx`-driven `subscribe_state()`** — listen for unsolicited TV broadcasts instead of 20 s polling. Deferred but load-bearing for state freshness.
- **Multi-transport single TV** — same panel via LG-network + CEC. v1 treats a CEC TV as an independent config entry; `get_connector_info()` resolution is the future hook.
- **Cosmetic** — `tisvlay-gui/ui/widgets.slint` hardcodes `is-lg`/`is-samsung`/`is-sony` vendor swatches; `cec` falls to the generic tile. One-line follow-up, not a blocker.
- **`.claude/CLAUDE.md`** — add `docs/cec/` to the Reference Documentation list once those docs carry content.

## Sequencing

Chunked so scaffolding lands with first use (no orphaned plumbing):

0. **Capability gating (prerequisite).** `capabilities` into `TvInfo` + GUI/tray/CLI gates. Independently useful; unblocks an honest CEC surface and ships first.
1. **Scaffold + power.** New crate, `linux-cec` dep (`default-features = false`), **`cargo-deny`/`about.toml` LGPL exception** (build-blocker — must be in this commit), root `Cargo.toml` membership, `CecVendorConfig`, `CecBackend` metadata, `Transport::Cec` end-to-end, registration, the `CecLink` seam + `FakeCecLink`, and `connect()` (with the standby-probe) / pairing / power on/off/state with tests. Compiles and controls power against the fake.
2. **Active-source input.** `switch_input()` + `get_state()` input fields + best-effort current-input, with tests.
3. **Discovery** — enumerate `/dev/cecN`; non-optional, so the device picker is the wizard's primary entry path (§ Prerequisite). May move earlier if the wizard needs it before manual entry is usable.
4. **NixOS enablement** — udev permissions + systemd-sandbox relaxation (both module.nix and the `.service`), module/tooling docs, `docs/cec/` (`protocol.md` + `testing.md`), and the `.claude/CLAUDE.md` Reference entry.
5. **Real-hardware bring-up** — once the USB adapter lands: field-test, then grow `docs/cec/firmware-quirks.md`.

## Migration on merge

Per [docs/plans/README.md](README.md): when this lands, fold the durable content out of this file before deleting it — the capability-gating gap and the always-reachable-transport / systemd-sandbox / LGPL gotchas belong in `docs/design.md` (or `docs/concerns/`) and `docs/cec/`, not evaporated with the plan.
