# Pulse-Eight autonomous mode: keep the TV's input label

A Pulse-Eight USB-CEC adapter left armed in autonomous mode while the host isn't driving it (reboot, shutdown, driver detach) shows up on the bus as its own device, and an LG TV then pins an LG-branded label ("LG DVD Player") on the PC's input that no later claim replaces. The evidence, mechanism and upstream history are in [docs/bugs/cec-pulse8-autonomous-label.md](../bugs/cec-pulse8-autonomous-label.md). This plan disarms the dongle when the driver disconnects, restoring the behavior upstream designed, and fixes the related gaps the investigation turned up: no CEC redial after the adapter returns, and no CEC claim logs.

## Decisions

- **Restore upstream's design, don't remove the feature.** The driver arms autonomous mode on purpose, so the adapter keeps working with the host's settings while the host is suspended; it's meant to disarm when unconfigured. The fix targets the disconnect path only. Suspend autonomy stays.
- **Verify by patching our own module build.** [packaging/nix/pulse8-cec.nix](../../packaging/nix/pulse8-cec.nix) already compiles the driver out of tree from `kernel.src`, so a `patches` entry (paths relative to the driver directory, `drivers/media/cec/usb/pulse8`) is all the change needs.
- **Nix-only for now.** No upstream patch until the fix has soaked. When it goes upstream, it goes to linux-media with a `Fixes: 323f52e02be6` tag, cc Hans Verkuil and Vicki Pfau.
- **One rebuild for verification.** The reboot capture (`boot.extraModprobeConfig = "options cec debug=2";`) ships in the same rebuild as the patched module, and is removed afterwards (level 2 logs every bus message).
- **Any autonomy knob is host configuration, never a TisVlay profile setting.** It's a driver-level, Pulse-Eight-specific setting, and applying it needs root, while the daemon runs sandboxed as the `tisvlay` user. Profiles stay vendor- and transport-agnostic behavior policy. If it's ever needed, it's a module parameter plus a NixOS module option next to `cec.pulse8.enable`, documented for other distros in [docs/ideas/cec-distro-setup.md](../ideas/cec-distro-setup.md).

## Trade-off

While the PC is **suspended**, the dongle still runs autonomously by design, so it can still create the LG-branded record. The disconnect fix covers reboots and shutdowns, not suspend. A suspend opt-out is the optional knob in step 7.

## Implementation

### 1. Driver patch: disarm on disconnect

- [ ] In `pulse8_disconnect`, send `SET_ACK_MASK 0` and `SET_AUTO_ENABLED 0` (under `pulse8->lock`, while `serio` is still set), then cancel the ping work and null `serio` as `323f52e02be6` does, so the oops fix stays intact.
- [ ] Add the patch to `pulse8-cec.nix` via `patches`, gated with `lib.versionAtLeast kernel.version "<TBD>"`: the patch's context only exists on kernels that contain `323f52e02be6`, and an older kernel (an LTS pin) would fail to build. The first release containing it is TBD; look it up before writing the gate.

### 2. Fallback: the daemon clears its claim on shutdown

Only if step 3 shows the tty can't carry writes inside `pulse8_disconnect`.

- [ ] On SIGTERM, the daemon clears the adapter's claim. The clear goes through a live serio, so the existing unconfigure path disarms the dongle.
- [ ] Order the daemon unit `After=` the inputattach unit, so it stops first at shutdown.
- [ ] Suspend doesn't stop the daemon, so suspend autonomy stays. A clean claim at the next boot also lets the TV re-detect the device and pick up our name.
- [ ] It can't cover an inputattach stop while the daemon keeps running (the reproducer), which isn't a real-world path.

### 3. Verification

- [ ] Before the patch: run the detach experiment (bug doc § Diagnostics) and confirm the label flips to "LG DVD Player".
- [ ] After the patch: rerun it; the label should stay on our name, and the TV shouldn't show the duplicate HDMI 1 entry.
- [ ] Warm reboot with the TV on: the label holds, and the debug capture shows which address the boot claim takes, and who acknowledged the polls if it isn't Playback 1. This also answers the bug doc's LA 12 question.

### 4. Redial CEC when the adapter returns

- [ ] Today the dial budget (`CONNECT_MAX_ATTEMPTS` × `CONNECT_BACKOFF` in `crates/tisvlay-world/src/systems/mod.rs`) runs out after about 9 s and nothing re-arms the dial, so a late or re-attached `/dev/cecN` leaves CEC down until a manual `reconnect` or the next wake.
- [ ] Choose between a capped indefinite backoff for CEC dials and a `/dev/cecN` watcher that re-arms the dial when the node appears. Open; decide when this step starts.

### 5. Logs

- [ ] Info-level log on each CEC claim: the logical address claimed and the OSD name advertised (or none).
- [ ] Info-level log for a failed CEC connect, with the error.

### 6. Doc corrections

- [ ] [docs/cec/protocol.md](../cec/protocol.md) § "Advertising our own name (the reverse direction)": a rename applies on the reconnect it forces (not "on the next wake"); a TV-side record formed while the dongle was autonomous persists until the TV re-detects the device (a replug), whatever we advertise afterwards.
- [ ] [docs/book/src/guide/configuration.md](../book/src/guide/configuration.md), at the `advertise_osd_name` paragraph: a troubleshooting note for a TV that shows a different name for the input (for a Pulse-Eight adapter with an LG TV, "LG DVD Player"), and the replug that clears it.
- [ ] Move the bug doc to Closed once step 3 passes.

### 7. Optional, later

- [ ] A `cec.pulse8.autonomous` NixOS option backed by a new module parameter, for users who'd rather give up suspend autonomy than risk the label after a suspend.
- [ ] An upstream note for linux-cec: `all_device_types` is never filled, so CEC 2.0 `<Report Features>` advertises no device types, and there's no way to set `cec_version`. It can go with the pending [OSD-name issue draft](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md).

## Open questions

- The first kernel release containing `323f52e02be6` (for the step 1 gate).
- Whether the tty can carry writes inside `pulse8_disconnect` (decides between step 1 and step 2).
- Why this morning's boot claim landed on LA 12 (step 3's reboot capture).
