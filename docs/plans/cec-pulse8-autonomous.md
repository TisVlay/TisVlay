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

- [x] In `pulse8_disconnect`, after cancelling the ping work and under `pulse8->lock`, send `SET_ACK_MASK 0` and `SET_AUTO_ENABLED 0` while `serio` is still set (only when the dongle is armed), then null `serio` as `323f52e02be6` does, so the oops fix stays intact. A failed disarm logs at info level: [pulse8-cec-disarm-on-disconnect.patch](../../packaging/nix/pulse8-cec-disarm-on-disconnect.patch).
- [x] Apply it in `pulse8-cec.nix`'s `postPatch`, gated on the source rather than a kernel version: only when the driver has the `-ENODEV` guard that `323f52e02be6` added. The commit first shipped in v7.1 and carries no stable Cc, so a version gate would miss an automatic stable backport. Drivers without it still disarm on their own and skip the patch. Built and checked against the 7.3 kernel: the patch applies and the module contains it.

### 2. Fallback: the daemon clears its claim on shutdown

Not needed. It was the alternative if the tty couldn't carry writes inside `pulse8_disconnect`; with `pulse8_cec` debug at 2, an inputattach stop logs both disarm commands going out (bug doc, step 12). The idea stays on record: the daemon clears its claim on SIGTERM, with its unit ordered `After=` the inputattach unit so it stops first, which would disarm through a live serio on any distro without a driver patch.

### 3. Verification

- [x] Before the patch: the detach experiment (bug doc § Diagnostics) flips the label to "LG DVD Player" after about 60 s, with a duplicate HDMI 1 entry on the TV, and only a replug restores it (bug doc, steps 5–7).
- [x] After the patch: no duplicate entry and no LG label. The label passes through "HDMI 1" and the disarmed dongle's own "CEC Adapter", and returns to our name on the reconnect, with no replug (bug doc, steps 10–12). The label doesn't stay on our name during the detach, which the plan expected; the disarmed firmware still answers the TV with its own identity. That's acceptable, since it doesn't pin the record.
- [x] Warm reboot with the TV on: the shutdown disconnect logs both disarm commands, the boot claim takes Playback 1 (every poll NACKed), and the label holds (bug doc, step 13). The LA 12 claim didn't recur, so the debug capture had nothing to explain; the bug doc keeps it as not reproduced.

### 4. Redial CEC when the adapter returns

Moved to [cec-device-presence.md](cec-device-presence.md): a device-presence model fed by udev events, with a capped backoff as the safety net.

### 5. Logs

Moved to [cec-device-presence.md](cec-device-presence.md), § Logs.

### 6. Doc corrections

- [x] [docs/cec/protocol.md](../cec/protocol.md) § "Advertising our own name (the reverse direction)": a rename applies on the reconnect it forces (not "on the next wake"); a TV-side record formed while the dongle was autonomous persists until the TV re-detects the device (a replug), whatever we advertise afterwards.
- [ ] [docs/book/src/guide/configuration.md](../book/src/guide/configuration.md), at the `advertise_osd_name` paragraph: a troubleshooting note for a TV that shows a different name for the input (for a Pulse-Eight adapter with an LG TV, "LG DVD Player"). With the patched module the label recovers on the next connect; on an unpatched driver (other distros), an HDMI replug clears it. Held until the CEC follow-ups near their end, so the user guide is edited once.
- [x] Move the bug doc to Closed once step 3 passes.

### 7. Optional, later

- [ ] A `cec.pulse8.autonomous` NixOS option backed by a new module parameter, for users who'd rather give up suspend autonomy than risk the label after a suspend. Note (2026-10-31): the driver's flag is the adapter's working state, and a sleeping adapter follows the autonomous flag in its EEPROM instead, so a parameter that only skips arming on claim wouldn't change the wake; libcec 8 writes the EEPROM flag (`am 0`), and the driver's `persistent_config=1` makes its EEPROM follow the claim ([tv-controls-pc.md](tv-controls-pc.md) step 8b).
- [ ] An upstream note for linux-cec: `all_device_types` is never filled, so CEC 2.0 `<Report Features>` advertises no device types, and there's no way to set `cec_version`. It can go with the pending [OSD-name issue draft](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md).

## Open questions

- Why the first boot's claim landed on LA 12. Not reproduced after the fix (bug doc, § Open questions); reopen if a claim lands on a Backup address again.
