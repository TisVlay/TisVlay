# CEC backend — protocol & command vocabulary

`tisvlay-cec` controls TVs over HDMI Consumer Electronics Control through the Linux CEC subsystem (`/dev/cecN`), built on Valve's [`linux-cec`](https://crates.io/crates/linux-cec) library (the safe `AsyncDevice` wrapper, tokio feature). This is the reference for *what tisvlay sends and how CEC maps onto the vendor-agnostic traits*; the crate layout and seam live in [testing.md](testing.md).

## The transport, and why it inverts the network model

A CEC adapter is a character device on the HDMI bus. Unlike the network backends, **the adapter is reachable even when the TV is in standby** — the bus stays powered, so we can query power and send commands to an "off" TV. Three consequences shape the backend:

- **Power-on is an on-bus command, not Wake-on-LAN.** Waking is `Image View On` (`AsyncDevice::wake`), which also broadcasts `Active Source` so the TV switches to our input. There is no MAC and no magic packet; `Capabilities::WOL` is deliberately *not* declared.
- **`Connected` and power are separate axes** — `CecConnector::reachable_in_standby()` returns `true`. Because the bus reaches the TV in standby, a standby TV is still *connected*; only its *power* is off. So `connect()` does **not** refuse a standby TV — it probes `Give Device Power Status` only to confirm the TV answers on the bus (a set powered off at the wall, or on a different HDMI input, drops off CEC and the probe fails, so `connect` fails and the TV reads disconnected), but `On` and `Standby` both connect. This mirrors what the hardware does: a TV powered off by its own remote keeps its connection (the monitor keeps hearing it), and a button-off does the same rather than dropping to `Connected:false`. The daemon's `power_on` therefore decides wake-vs-direct by the **reported power state**, not by whether `connect` succeeds: a connected-but-standby TV is woken (`Image View On`) exactly like an unreachable one. Crucially, when the TV is *already connected* in standby the daemon **wakes in place** — it sends the wake on the existing connection (`TvConnection::power_on`) and awaits the push subscription reporting it on, rather than tearing the connection down and re-opening a fresh link. The teardown would flap `Connected` (a spurious "Disconnected" on a power-*on*); wake-in-place keeps `Connected` steady, so the user only ever sees the "is awake" notification. If the in-place wake never confirms, `power_on` falls back to the full teardown + re-open. `manager.power_off` is the mirror: it keeps the connection for a `reachable_in_standby` backend (sending `Standby`, forwarding the new power state) instead of dropping it. Network backends leave `reachable_in_standby()` at its `false` default, so their connect-success still means "on" and `Connected` tracks power as before.
- **`wake_and_connect` re-sends until on.** It *re-sends* `Image View On` on every verify attempt — a single fire-and-forget frame is easily lost against a freshly-opened adapter (still claiming its logical address) or a just-standby TV — and polls power between sends until the TV reports on. A send failure re-opens the handle to re-claim a possibly-lost logical address before the next attempt.
- **No network monitoring.** `transport()` is `Transport::Cec`, so the daemon skips the carrier/route watchers it scopes to `Transport::Network`.

## Capability mapping

| tisvlay capability | CEC realization | Status |
|---|---|---|
| `POWER` on/off | `Image View On` (wake path) / `Standby` | ✅ |
| power **state** | `Give Device Power Status` → `Report Power Status` (`On` / `Standby` / `ToOn` / `ToStandby`) | ✅ |
| `INPUT` switch | `Set Stream Path` to a port's physical address | ✅ (switch); readback ❌ |
| `DISCOVERY` | enumerate `/dev/cecN` nodes (not a bus scan) | ✅ |
| `VOLUME` | only relative `User Control Pressed{Volume Up/Down}` exists; no absolute set, readback is an AVR feature | ❌ deferred (needs a relative-volume primitive) |
| `MUTE` | `User Control Pressed{Mute}` is a toggle; readback device-dependent | ❌ deferred |
| `SCREEN`-blank | no discrete CEC screen-off; **emulated via power** when `emulate_screen_via_power` (default on) | ✅ emulated |
| `BACKLIGHT` | not a CEC concept | ❌ omitted |

### Screen-via-power emulation

HDMI-CEC has no way to blank the panel while leaving the TV on, so a bare CEC backend would ignore the stock screen-blank profile actions (`on_dpms_off` / `on_idle_timeout` / `on_session_lock` default to `screen_off`, with `screen_on` mirrors). The `emulate_screen_via_power` config field (`[tv.cec]`, default `true`) closes that gap: `CecConnector::emulate_screen_via_power()` reports it, and the **daemon** — not the connection — advertises `SCREEN` (config-aware `get_capabilities`) and dispatches `screen_off`/`screen_on` as `power_off`/`power_on`. Living at the daemon dispatch is load-bearing: it reuses the correct reachable-in-standby power path (keep the connection, **synthesize** the standby cache the TV won't re-broadcast for a *commanded* standby, wake in place). Routing screen→power inside the connection instead would leave the cached power state stale-`Active` after a commanded standby, which would make a later `power_on` wrongly no-op. Opt out (`false`) to have the device ignore screen actions entirely — sub-stock fidelity for a user who'd rather not power-cycle a slow-to-wake panel on every idle blank. Default-on so the stock profiles make every device "go dark" (LG blanks its OLED; CEC powers off) consistently across a mixed fleet.

`IPV4` / `IPV6` are **not** declared — beyond "not a network transport", omitting them stops the daemon's manual-entry enrichment from ARP-probing a `/dev/cecN` path as if it were an IP.

## Message vocabulary

What the backend actually puts on the bus (`linux_cec::message::Message`), via the `CecLink` seam:

- **`ImageViewOn`** — wake. Sent through `AsyncDevice::wake(set_active = true, …)`, which appends `Active Source` so the TV both powers on and switches to us.
- **`Standby`** — power off, addressed to `LogicalAddress::Tv`.
- **`GiveDevicePowerStatus` → `ReportPowerStatus { status }`** — the power query, a `tx_rx_message` waiting for `Opcode::ReportPowerStatus` (1 s CEC ceiling). The operand maps to `PowerState`:

  | CEC `PowerStatus` | `PowerState` | note |
  |---|---|---|
  | `On` | `Active` | |
  | `Standby` | `Standby` | |
  | `ToOn` | `WarmingUp` | deliberate approximation — CEC's "in transition", not a projector warmup |
  | `ToStandby` | `CoolingDown` | same |

- **`SetStreamPath { address }`** — input switch, broadcast. Asks whichever device owns `address` to become the active source — i.e. the TV switches to that input.

### Physical addresses

A TV is the bus root, physical address `0.0.0.0`. Its HDMI port *N* is `N.0.0.0` = `N << 12` (so `HDMI_1` → `0x1000`, `HDMI_2` → `0x2000`, …). `switch_input("HDMI_N")` parses N (valid 1–15) into that physical address and sends `Set Stream Path`. `list_inputs` returns a fixed best-effort `HDMI 1`–`HDMI 4` set — **CEC cannot enumerate the TV's actual ports** — and `get_current_input` is always unknown, since CEC has no portable "which input is showing" query.

The address above is the *target* of a `Set Stream Path`. Distinct from it is the address the **adapter itself claims** when a link opens. A USB adapter advertises `CEC_CAP_PHYS_ADDR`, so the kernel can't derive its address from a DRM connector's EDID the way GPU DisplayPort-CEC does — `CecLink::open` therefore sets `CecVendorConfig::physical_address` (default `1.0.0.0`, i.e. the adapter is on the TV's HDMI 1) *before* claiming the Playback logical address. Without a valid physical address the logical-address claim never takes effect on the bus and no transmit lands, so this is load-bearing for USB adapters; the leading digit must match the TV input the adapter is plugged into. On a GPU DP-CEC output the cap is absent (the kernel already assigned an address from the connector), so `open` skips the write and the field is ignored. This mirrors Valve's `cecd`, which gates the same `set_physical_address` on `CEC_CAP_PHYS_ADDR`.

## State updates (push)

CEC is a **push backend**, not polled: `subscribe_state` opens a *second*, follower-mode handle on the same `/dev/cecN` node and runs a receive loop that maps unsolicited traffic — `Report Power Status`, `Standby`, `Active Source`, `Image View On`/`Text View On` — onto a running `TvState` snapshot pushed on every change. So power state tracks the TV in real time, including changes made from the TV's own remote, and the daemon's 20 s poll loop skips CEC.

Why a second handle: `AsyncDevice`'s worker is a single serial thread, so a blocking receive loop on the tx handle would starve `power_off`/`switch_input`/power queries. CEC allows multiple opens and follower/initiator mode is per-fd, so the receive fd listens independently of the tx fd (it claims no logical address of its own — the tx handle already claimed one for the adapter). This is the same follower-mode receive design `cecd` uses. The monitor task is aborted when the connection drops, closing the state channel — that closure is what drives the daemon's subscription-end `ConnectionChanged(false)` on power-off (the same path LG's socket-close takes). What unsolicited messages a given TV actually emits is TV-dependent, so real-time remote-driven updates are best-effort.

## Device identity (`HardwareId` + vendor tile)

The bus answers `Give OSD Name` (the TV's own name, e.g. `OLED42C3PU`) and `Give Device Vendor ID` (a 24-bit OUI) — even in standby. The CEC identity is **the cross-vendor `HardwareId`**, not a parallel structure: `identity::query` maps OSD name → `model`, resolved vendor → `manufacturer` (no serial over CEC). `CecConnector::connect` runs it once (bounded — see below) and hands it to `CecConnection`, which returns it synchronously from `hardware_id()` — exactly like LG's cached `getSystemInfo`. So it flows through the *existing* `Tv1.HardwareId` → the GUI's **Make / Model** rows + the header model, refreshed on a dongle-swap when the GUI re-fetches on `ConnectionChanged`.

`linux-cec`'s `VendorId` is only the raw OUI (`Display` = `00-e0-91`), with no name table, so `identity.rs` owns a small OUI→name map (libcec's `CEC_VENDOR_*` values). A resolved OUI gives the `manufacturer` name (`LG`); an unresolved one falls back to the raw `00-e0-91`. The map also yields a **canonical id** (`lg`/`samsung`/…) for branding — the connection returns it from `brand()`, and the daemon exposes it as `Tv1.Brand` (falling back to the backend vendor id, so an unresolved CEC TV brands as the generic `CE` tile).

**Discovery probes identity.** `CecDiscovery` opens each `/dev/cecN` and runs the same query, so the pairing wizard's list shows the TV's own name (`OLED42C3PU`) and Make/Model, its name field prefills from the OSD name, and the row's vendor tile brands by the resolved vendor. A node that can't be opened (busy) or answers nothing falls back to the bare `HDMI CEC (/dev/cecN)` entry. The pair flow keeps a fallback: a blank friendly name defaults to the connection's `HardwareId` model. The canonical brand rides `DiscoveredTv.vendor_data["brand"]` — the free-form map that already crosses D-Bus for autofill, so the tile brands without changing `DiscoveredTv`'s wire signature (`vendor` stays the backend id `cec`, used for pairing).

**Bounded, best-effort.** The identity queries use a short `IDENTITY_QUERY_TIMEOUT` (2 s), not the `Timeout::MAX` power uses — a TV that ignores `Give OSD Name` can't stall connect or discovery. (The C3 answered these *in Standby*, so the on-connect/discovery capture works regardless of power.) A subtle prior bug: the raw `AsyncDevice::get_osd_name` returns the *local adapter's* fixed 14-byte NUL-padded buffer, and an embedded NUL is an invalid D-Bus string that drops the daemon's name — hence the bus query (variable-length, no padding) plus `identity::sanitize`.

## Discovery

CEC "discovery" is not a bus scan: it lists the host's `/dev/cecN` character devices (`CecDiscovery`), one `DiscoveredTv` per node, so the pairing wizard offers a device picker. The device path rides `vendor_data["device"]`, and the schema's `autofill = "discovery.device"` pre-fills the `device` config field. Each node is then *probed* for its identity (OSD name + vendor) — see § Device identity — so the list shows the TV's real name/model and the name field prefills; a node that can't be opened (busy, held by an active connection) or answers nothing falls back to the bare `HDMI CEC (/dev/cecN)` entry.

## Pairing

There is none, really: any device on the bus is reachable, so `start_pairing` validates the config parses and completes immediately with empty credentials — the trivial end of the `PairingState` machine.

## Hardware reality

CEC needs the kernel to expose a `/dev/cecN` node, which needs both the `cec` core module *and* a driver that registers an adapter. Most GPU HDMI ports don't wire the CEC pin. Known-working sources: an active **DisplayPort→HDMI adapter on an amdgpu/i915 output** (the GPU driver tunnels DP-AUX CEC — **nvidia does not**), or a **USB CEC adapter** (e.g. Pulse-Eight via the kernel `pulse8-cec` driver). Device access + the daemon-sandbox relaxation are gated behind the NixOS module's `services.tisvlay.cec.enable` (udev rule + `DeviceAllow=char-cec rw`); see [docs/licensing.md](../licensing.md) for the LGPL note on the `linux-cec` dependency.

## Quirks

A `firmware-quirks.md` (Symptom → Root Cause → Fix → Regression-Coverage) will grow here as real-hardware field testing surfaces issues — none recorded yet, since validation waits on a CEC adapter that exposes `/dev/cecN`.
