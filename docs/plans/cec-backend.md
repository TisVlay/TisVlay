# CEC backend (`tisvlay-cec`)

A new vendor backend that controls TVs over HDMI-CEC through the Linux CEC subsystem (`/dev/cecN`), using Valve's [`linux-cec`](https://gitlab.steamos.cloud/holo/linux-cec/) library. It is the first non-network, non-vendor-protocol backend — `Transport::Cec` — and the first to exercise the trait surface's transport-agnostic promise end to end.

## Goal & v1 scope

Ship a CEC backend whose **v1 surface is power + active-source input**: turn the TV on/off, read its power state, and switch the TV to a given HDMI input (primarily "switch to the input our adapter is plugged into"). Volume, mute, backlight, and OLED screen-blank are explicitly **out of v1** (see § Capability mapping for why). The backend is built and tested **without a real TV-CEC link**, because no such link exists on the development machine today (see § Hardware reality) — real-hardware validation is a later, parallel track gated on a USB CEC adapter.

## Locked decisions

These three were settled up front; the rest of the plan builds on them.

1. **Integration = the `linux-cec` library, statically linked** (its `async` / tokio `AsyncDevice` wrapper). Consumed as a git-pinned or vendored dependency (not on crates.io). Rationale: it fits our architecture exactly — the backend owns its `/dev/cecN` handle, and `tisvlayd` is the natural single owner of the CEC adapter, so we don't need `cecd`'s multi-app arbitration. Standing up `cecd` as a second system daemon was rejected: its own wake/suspend-sync overlaps tisvlay's power management (conflict risk), its D-Bus surface is coarser than the library, and it isn't packaged for our targets. The raw `linux-cec-sys` ioctl layer (BSD-3) was rejected as reinventing the message/operand codec the library already provides.
   - **License note:** tisvlay is Apache-2.0; `linux-cec` is LGPL-2.1-or-later. Static-linking it carries the LGPL §6 relink obligation — low-friction for an open-source project (the source is already available to relink against), but worth a line in packaging docs, and a reason to revisit if tisvlay ever ships a closed binary. `linux-cec-sys` alone is BSD-3 if we ever need to shed the LGPL dependency.

2. **v1 capabilities = `POWER | INPUT`.** No `VOLUME`, `MUTE`, `BACKLIGHT`, or `SCREEN`. Relative volume (which is the only volume CEC portably offers) is a deferred, cross-cutting capability-model extension, not part of this plan.

3. **Software-first, fake/virtual CEC for tests.** Unit tests run against a fakeable seam at the `linux-cec` boundary; optional integration tests run against the kernel `vivid` virtual CEC adapter. Real hardware (the incoming USB CEC adapter) and its NixOS enablement are a separate, later track.

## Hardware reality (development machine)

Recorded so the plan's "software-first" stance is legible and we don't re-investigate:

- The TV is driven from `card1-DP-3` via a UGREEN active DP→HDMI adapter on the **NVIDIA open kernel module** (595.45.04). No nvidia module references any `cec` / `drm_dp_cec` symbol — **nvidia-drm does not implement DP-AUX CEC tunneling** (amdgpu and i915 do; nvidia, open and proprietary, does not). There is no `/dev/cec*`, and this cannot be fixed in software.
- The i9-14900K **does** contain an iGPU (UHD 770), but it is **disabled in UEFI** (PCI `00:02.0` absent). Even enabled, it isn't a clean CEC path: the TV's video must stay on nvidia for gaming, and CEC can't ride the nvidia cable — so the iGPU could only contribute CEC via a second cable to a spare TV input (and this board exposes the iGPU DP only over Thunderbolt USB-C). `i915` (which *does* tunnel DP-CEC) is in the kernel build, making the iGPU the only no-kernel-rebuild route to a real `/dev/cec` if we ever want one for development.
- The kernel **`cec` core module is available**. `vivid` (virtual CEC, for tests) and `pulse8-cec` (likely driver for the incoming USB adapter) are **not** in the current NixOS kernel build — both are enablement steps.
- **Production CEC path = the USB CEC adapter**, which joins the TV's CEC bus over USB↔HDMI independently of which GPU draws video.

## Capability mapping (what CEC can and can't do)

| tisvlay capability | CEC reality | v1 |
|---|---|---|
| `POWER` on/off | `ImageViewOn` (on) / `Standby` (off) — reliable | ✅ |
| power **state** | `GiveDevicePowerStatus` → `ReportPowerStatus` (`On` / `Standby` / `InTransitionStandbyToOn` / `InTransitionOnToStandby`) maps cleanly onto `PowerState`, including the transient `WarmingUp` / `CoolingDown` | ✅ |
| `INPUT` switch | `ActiveSource` / `SetStreamPath` to a physical address — reliable for "switch the TV to this HDMI input"; rich input **enumeration / current-input readback** is not portable | ✅ (switch), best-effort readback |
| `VOLUME` | No absolute set. Only **relative** `UserControlPressed{VolumeUp/VolumeDown}`. Absolute readback via `ReportAudioStatus` is device-dependent (an AVR/soundbar feature). Needs a new relative-volume primitive | ❌ deferred |
| `MUTE` | `UserControlPressed{Mute}` is a **toggle**, not set-state; readback device-dependent | ❌ deferred |
| `BACKLIGHT`, `SCREEN`-blank | Not CEC concepts | ❌ omit |

Two model notes that shape the impl:

- **Power-on is the connector's `wake()` path, not a `TvConnection` method.** Unlike a network TV (unreachable when off, hence WoL), the CEC adapter is always on the bus, so the device handle is always reachable. `wake()` sends `ImageViewOn` (+ `ActiveSource`); `wake_and_connect()` is `connect()` then `wake()` with a `ReportPowerStatus == On` verify. `supports_wake()` is always `true`. `power_off()` sends `Standby`. Set the `POWER` capability but **not** `WOL` (CEC wake is not a magic packet).
- **`subscribe_state()` is `None` in v1 (poll-based `get_state()`).** CEC TVs do broadcast unsolicited `Standby` / `ActiveSource` / `ReportPowerStatus`, so a future `rx`-driven subscription is possible — noted as deferred, not built.

## Crate design — `crates/tisvlay-cec/`

Follows the one-trait-impl-per-file convention from [docs/backends/implementing-a-vendor.md](../backends/implementing-a-vendor.md), cloning `tisvlay-mock` as the skeleton (skip its `handle.rs` / `state.rs` / `pairing.rs` test-driving scaffolding — a real backend talks to hardware).

- **`backend.rs`** — `CecBackend`: `vendor_id()` = `"cec"`, `vendor_name()` = `"HDMI CEC"`, `capabilities()` = `POWER | INPUT` (no `IPV4`/`IPV6` — not a network transport). Override `verify_retry_config()` for CEC bus latency.
- **`config.rs`** — `CecVendorConfig`: the `/dev/cecN` device path (UserInput, primary descriptor); the TV's discovered physical address and bound DRM connector name (Diagnostic); optional OSD name we advertise. Identity hook: `linux-cec`'s `get_connector_info()` ties a `/dev/cecN` to a DRM connector, which dovetails with the daemon's existing EDID connector-matching in [desktop_probe.rs](../../crates/tisvlayd/src/desktop_probe.rs).
- **`connector.rs`** — `CecConnector`: `transport()` = `Transport::Cec` (daemon already skips network/carrier monitoring for non-network transports); `survives_source_ip_change()` = `false` (n/a). `connect()` opens the device, claims a `playback` logical address, and caches adapter + connector info. `start_pairing()` has **no real auth** — it validates the device opens and probes the bus for the TV's physical address, then emits `Complete` with empty credentials immediately (the trivial end of the pairing state machine). `build_config_from_pairing()` persists the device path plus discovered diagnostics. `config_schema()` declares the fields above.
- **`connection.rs`** — `CecConnection`: `power_off()` (`Standby`), `get_power_state()` (map `ReportPowerStatus`), `switch_input()` (`SetStreamPath` / `ActiveSource` to the target physical address), best-effort `get_current_input()`. `get_state()` assembles a `TvState` from power + active-source. Everything else stays at the `NotSupported` default.
- **`discovery.rs`** *(optional, feature-gated like LG's)* — enumerate `/dev/cecN`, read each adapter's connector / OSD / vendor, return `DiscoveredTv { address = device path, … }` so the pairing wizard gets a device picker. Lean: implement the minimal enumerate behind a `discovery` feature; acceptable to defer to a later commit.
- **`device.rs` (the seam)** — a thin internal trait (e.g. `CecLink`) wrapping exactly the `linux-cec` calls we use (open, set logical address, tx `Message`, tx-rx, power/active-source helpers). Production impl wraps `AsyncDevice`; tests use a scripted `FakeCecLink`. `linux-cec` ships no fake server, so this seam is our testability boundary — the CEC analog of LG's `FakeSsapServer`, one layer lower.
- **`lib.rs`** — `VENDOR_ID` / `VENDOR_NAME` constants, re-exports, production/test factory fns.

## Registration

Three edits in `tisvlay-backends`, cloning `register_lg`: add the `tisvlay-cec` dependency in its `Cargo.toml`; add `register_cec()` in [lib.rs](../../crates/tisvlay-backends/src/lib.rs) (pass `discover_fn = None` until discovery lands); call it from `create_registry()` (and `create_test_registry()` if multi-vendor lookup tests need it). No other crate changes.

## Testing strategy

- **Seam-level unit tests** drive `CecConnector` / `CecConnection` through a scripted `FakeCecLink`: power on/off/state transitions, active-source switch, and fault injection (device gone mid-call, bus busy, no TV present, transition-in-progress power states).
- **Layered** like the LG guide recommends: config parse → message/flow mapping → connector pairing/connect → connection power/input → fault injection.
- **Optional `vivid` integration tests**, feature-gated and skipped when the module is absent, for real-ioctl coverage against the kernel's virtual CEC adapter.
- Daemon-level integration tests continue to use `tisvlay-mock`, not the real CEC backend.

## NixOS / packaging enablement (minimal in this plan)

Kept deliberately narrow here (the broader story is a separate packaging task, like the tabled firewall/SSDP work):

- **Permissions** — `/dev/cecN` is not world-accessible; `tisvlayd` runs as the `tisvlay` user. Ship a udev rule (or group membership) granting that user access, wired into [packaging/nix/module.nix](../../packaging/nix/module.nix).
- **Kernel modules** — document that real hardware needs `cec` (available) plus the adapter driver (`pulse8-cec` for the USB unit), which the current NixOS kernel build lacks; and `vivid` for virtual-CEC tests. Enablement instructions, not a hard module dependency.
- **Debug tooling** — `v4l-utils` (`cec-ctl`) as a devshell convenience.

## Risks & deferred work

- **Dependency provenance / LGPL** — pin `linux-cec` to a git rev or vendor it; carry the LGPL static-link note in packaging.
- **Weak input readback** — set expectations: "switch the TV to our input" is the reliable operation; current-input / input-list are best-effort.
- **Real-hardware lag** — validation waits on the USB adapter + NixOS module enablement; until then, fake/`vivid` only.
- **Relative volume** — a future cross-cutting capability (`Capabilities` bit + `TvConnection` methods + D-Bus + CLI + GUI). Its own plan.
- **`rx`-driven `subscribe_state()`** — listen for unsolicited TV broadcasts instead of polling. Deferred.
- **Multi-transport single TV** — the same panel reachable via both the LG-network backend and CEC. v1 treats a CEC TV as an independent config entry; `get_connector_info()` is the future correlation hook.

## Sequencing

Chunked so scaffolding lands with its first real use (no orphaned plumbing):

1. **Scaffold + power.** New crate, `linux-cec` dependency, `CecVendorConfig`, `CecBackend` metadata, `Transport::Cec` end-to-end, registration, the `CecLink` seam + `FakeCecLink`, and `connect()` / pairing / power on/off/state with tests. Compiles and controls power against the fake.
2. **Active-source input.** `switch_input()` + `get_state()` input fields + best-effort current-input, with tests.
3. **Discovery** *(optional)* — enumerate `/dev/cecN` behind a `discovery` feature.
4. **NixOS enablement** — udev permissions, module/tooling docs, `docs/cec/` (protocol + testing).
5. **Real-hardware bring-up** — once the USB adapter lands: field-test, then grow `docs/cec/firmware-quirks.md`.
