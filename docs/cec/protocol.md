# CEC backend — protocol & command vocabulary

`tisvlay-cec` controls TVs over HDMI Consumer Electronics Control through the Linux CEC subsystem (`/dev/cecN`), built on Valve's [`linux-cec`](https://crates.io/crates/linux-cec) library (the safe `AsyncDevice` wrapper, tokio feature). This is the reference for *what tisvlay sends and how CEC maps onto the vendor-agnostic traits*; the crate layout and seam live in [testing.md](testing.md).

## The transport, and why it inverts the network model

A CEC adapter is a character device on the HDMI bus. Unlike the network backends, **the adapter is reachable even when the TV is in standby** — the bus stays powered, so we can query power and send commands to an "off" TV. Three consequences shape the backend:

- **Power-on is an on-bus command, not Wake-on-LAN.** Waking is `Image View On` (`AsyncDevice::wake`), which also broadcasts `Active Source` so the TV switches to our input. There is no MAC and no magic packet; `Capabilities::WOL` is deliberately *not* declared.
- **`connect()` must refuse a standby TV.** The daemon's `power_on` decides direct-connect-vs-wake by whether `connect()` succeeds, and the `/dev/cecN` handle opens whether or not the TV is on. So `CecConnector::connect` probes `Give Device Power Status` and returns `Err` unless the TV reports `On`/`In Transition to On` — otherwise `power_on` would take the direct branch and never send `Image View On`. `wake_and_connect` sends `Image View On` and polls power until the TV reports on.
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
| `BACKLIGHT`, `SCREEN`-blank | not CEC concepts | ❌ omitted |

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

## Discovery

CEC "discovery" is not a bus scan: it lists the host's `/dev/cecN` character devices (`CecDiscovery`), one `DiscoveredTv` per node, so the pairing wizard offers a device picker. The device path rides `vendor_data["device"]`, and the schema's `autofill = "discovery.device"` pre-fills the `device` config field. It is enumeration only — it does not open a node or read the TV's OSD name (that needs the `/dev/cecN` access the daemon, not a pre-pair scan, holds).

## Pairing

There is none, really: any device on the bus is reachable, so `start_pairing` validates the config parses and completes immediately with empty credentials — the trivial end of the `PairingState` machine.

## Hardware reality

CEC needs the kernel to expose a `/dev/cecN` node, which needs both the `cec` core module *and* a driver that registers an adapter. Most GPU HDMI ports don't wire the CEC pin. Known-working sources: an active **DisplayPort→HDMI adapter on an amdgpu/i915 output** (the GPU driver tunnels DP-AUX CEC — **nvidia does not**), or a **USB CEC adapter** (e.g. Pulse-Eight via the kernel `pulse8-cec` driver). Device access + the daemon-sandbox relaxation are gated behind the NixOS module's `services.tisvlay.cec.enable` (udev rule + `DeviceAllow=char-cec rw`); see [docs/licensing.md](../licensing.md) for the LGPL note on the `linux-cec` dependency.

## Quirks

A `firmware-quirks.md` (Symptom → Root Cause → Fix → Regression-Coverage) will grow here as real-hardware field testing surfaces issues — none recorded yet, since validation waits on a CEC adapter that exposes `/dev/cecN`.
