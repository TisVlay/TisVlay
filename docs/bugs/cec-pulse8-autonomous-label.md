# Pulse-Eight autonomous mode pins the TV's input label

**Status:** open (2026-10-24). Fix plan: [docs/plans/cec-pulse8-autonomous.md](../plans/cec-pulse8-autonomous.md).

For an LG C3 on a Pulse-Eight USB-CEC adapter (`pulse8-cec`, firmware `000c` built 2020-04-28, Linux 7.3.0, tisvlay 0.4.0 at `a59c4eee`), the TV labels the PC's HDMI 1 input **"LG DVD Player"** while the kernel advertises `OSD Name : 'TisVlay'`. The fix from [PR #197](https://github.com/TisVlay/TisVlay/pull/197) is in place and working: the kernel answers `<Give OSD Name>` with our name, and the TV acknowledges it. The TV keeps its own label anyway.

The ground truth for "what the TV shows" is the LG TV's own input list over SSAP (`ssap://tv/getExternalInputList`), which the LG backend subscribes to; the CEC TV's `state/input` is synthetic (`HDMI 1`–`HDMI 4`) and says nothing about the TV's labels:

```fish
tisvlay --json list | jq -r '.["<lg-uuid>"]."state/input".available[] | select(.id=="HDMI_1") | .label'
```

## Summary

While the host isn't driving the dongle (driver detached, reboot, shutdown), the Pulse-Eight firmware runs in **autonomous mode** with the settings the last claim gave it. It shows up on the bus as a device of its own, echoes the TV's LG vendor ID, and the TV stores an LG-branded record for the port ("LG DVD Player"). That record survives every later claim, including a correct `<Set OSD Name>` reply and a different vendor ID; only re-detecting the device (a physical HDMI replug) clears it. Once the record is clean, renames follow the reconnect each rename forces.

The dongle is left armed on disconnect because the driver can no longer reach it at that point (§ Mechanism). Upstream's design disarms on unconfigure and keeps autonomy for suspend; the disconnect-time disarm most likely stopped reaching the dongle with `323f52e02be6` (2026-03). That last step is inferred from the code and unproven on hardware.

## Timeline (2026-10-24)

Times are local (CDT). Bus bytes are `<initiator><destination> <opcode> <operands>`; `4` is our Playback 1 address, `0` the TV, `f` broadcast.

1. **Boot (09:51:34, warm reboot from 09:50:51).** The daemon started at 09:52:08; `/dev/cec0` registered about 3 s later. The claim landed on **LA 12 (Backup 1)**, not Playback 4. Address 8 is the Chromecast; 4 and 11 must have looked taken. The kernel logged no `polling for LA … failed` lines (printed at `dprintk(0)`, so always), so the polls were *acknowledged*, not errored. The TV (CEC 1.4) has no Backup addresses. HDMI 1 read "LG DVD Player".
2. **Daemon restarts (10:28, 10:31).** Each claim polled `44`, got NACK (`status 0x04`), and claimed LA 4. The 10:31 trace: `4f 84 10 00 04` (Report Physical Address 1.0.0.0, Playback), then the TV's `04 46` (Give OSD Name) answered by `40 47 54 69 73 56 6c 61 79` ("TisVlay", `status 0x01`, ACKed). The label stayed "LG DVD Player".
3. **Rename to "Spam" in the GUI (10:33:46).** The vendor-field change forced a reconnect; the kernel carried `'Spam'` at LA 4. The TV kept "LG DVD Player" until the HDMI cable was pulled from the adapter and plugged back, then showed "Spam".
4. **Rename back to "TisVlay" (10:37:48).** The TV followed with no replug. With a clean record, a rename applies on the reconnect it forces.
5. **Detach experiment (10:56:11).** `systemctl stop pulse8-cec-inputattach@ttyACM0` with the daemon running. The kernel logged `new physical address f.f.f.f`. The SSAP label read "TisVlay" through 10:57:04 and "LG DVD Player" from 10:57:14. Watching the TV's input menu, the user saw a duplicate HDMI 1 entry appear next to "TisVlay": "Recorder", then "CEC Device", then the two collapsed into "LG DVD Player".
6. **Re-attach (10:58:17).** The adapter re-registered (`serio1`), but the CEC TV stayed `connected: false, reconnecting: false` and the daemon logged nothing (§ Related findings). A manual `tisvlay reconnect` at 11:05:13 claimed LA 4 cleanly (`44` NACKed); the label stayed "LG DVD Player".
7. **Detach, wait 90 s, re-attach, reclaim at once (11:11:40, `serio2`).** Meant to mimic the boot's 3 s gap between registration and claim. Claimed LA 4 again, so it didn't reproduce step 1. On the TV, the duplicate cycle repeated, then "TisVlay" briefly replaced "LG DVD Player" before the label settled back on "LG DVD Player".
8. **Topology scan.** `cec-ctl -d /dev/cec0 -S` found only the TV (0, LG `00 e0 91`, `OLED42C3PU`), us (4, `1.0.0.0`) and the Chromecast (8, `4.0.0.0`). No live phantom: the second device exists only in the TV's memory.
9. **Non-LG vendor ID.** `cec-ctl -d /dev/cec0 --playback --osd-name TisVlay --vendor-id 0x8c2daa` claimed LA 4 with that vendor and name. 20 s later the label was still "LG DVD Player". CEC debug was off, so it's unconfirmed that the TV asked for the vendor ID during this claim; it did on every traced claim before.

## Mechanism

The driver's own notes (`drivers/media/cec/usb/pulse8/pulse8-cec.c`, header comment) describe the firmware in autonomous mode:

> - In autonomous mode, only messages from a TV will be acknowledged, even polling messages. Upon receiving a message from a TV, the dongle will respond to messages from any logical address.
> - In autonomous mode, the dongle will by default reply Feature Abort [Unrecognized Opcode] when it receives Give Device Vendor ID. It will however observe vendor ID's reported by other devices and possibly alter this behavior. When TV's (and TV's only) report that their vendor ID is LG (0x00e091), the dongle will itself reply that it has the same vendor ID, and it will respond to at least one vendor specific command.

How the dongle gets there and stays there:

- **Every claim arms it.** `pulse8_cec_adap_log_addr` sets the ACK mask for the claimed address, then sends `SET_AUTO_ENABLED 1`, `SET_LOGICAL_ADDRESS_MASK` (all Playback addresses, 4|8|11, deduced from the primary device type), `SET_DEFAULT_LOGICAL_ADDRESS`, the physical address and the OSD name. That happens with `persistent_config=0` too: the parameter only gates the EEPROM write (`pulse8_ping_eeprom_work_handler`) and restoring the stored config at probe. The settings sit in the dongle's RAM, which a warm reboot doesn't clear (USB power stays on).
- **Unconfigure disarms it, but only through a live serio.** `pulse8_cec_adap_log_addr(CEC_LOG_ADDR_INVALID)` sends `SET_ACK_MASK 0` and `SET_AUTO_ENABLED 0`. Daemon restarts reach this path (the connection-holding opens clear a held claim first), which is why later claims poll cleanly.
- **Disconnect skips the disarm.** `pulse8_disconnect` cancels the ping work and sets `pulse8->serio = NULL` *before* `cec_unregister_adapter`. Unregistering resets the physical address and unconfigures the adapter, but `pulse8_send_and_wait_once` now returns `-ENODEV`, so the disarm never reaches the dongle. Without the driver's pings (every 15 s, `PING_PERIOD`), the firmware leaves controlled mode with auto enabled; the label flipped about 60 s after the detach in step 5.

What the TV makes of it (an interpretation of the observed menu behavior): the autonomous dongle appears as an extra device on `1.0.0.0` ("Recorder" fits the firmware's default type; LA 1 is Recorder 1), echoes LG's vendor ID, and the TV merges it into the port's record under an LG device name. When we claim again, our name shows briefly, then the merged record's LG-derived name wins. Nothing we send afterwards replaces that record.

## Upstream history

- `3dff3106fd79` (2016-07): the driver, in staging.
- `e28a6c8b3fcc` (2016-08, Johan Fjeldtvedt), "sync configuration with adapter": configuration changes are written to the adapter so it "can continue operating in autonomous mode with the same settings when it is disconnected from the driver (typically by going into suspend)". Also: "When a new configuration is written, the autonomous mode is automatically enabled. When the device is unconfigured, autonomous mode is disabled." Added `persistent_config` (on by default).
- `89c2e228718c` (2016-08): the autonomous-mode notes quoted above.
- `58584b28a8e5` (2016-09), "store logical address mask": also sets the logical-address mask, which (not the ACK mask) is what autonomous mode uses.
- `9b7c0c476f66` (2017-07, Hans Verkuil), "persistent_config should be off by default": the driver should come up unconfigured like other CEC drivers. It doesn't change the in-RAM arming on claim.
- `323f52e02be6` (2026-03, Vicki Pfau), "Handle partial deinit": fixes an oops when the CEC node is held open across a disconnect (the daemon holds it open). It nulls `serio` before `cec_unregister_adapter` and guards the send path with `-ENODEV`. From the code, this is the change that stops the unconfigure-time disarm from reaching the dongle on disconnect. Before it, the disarm would have been sent while `serio` was still set, unless the tty was already unusable inside disconnect. Unverified on hardware.

So upstream's intended behavior is: armed while the host is suspended, disarmed when unconfigured. A reboot or an inputattach stop is a disconnect, not a suspend, and should disarm.

## Related findings

- **CEC never redials after the adapter returns.** The dial budget is `CONNECT_MAX_ATTEMPTS` (3) at `CONNECT_BACKOFF` (3 s) ([crates/tisvlay-world/src/systems/mod.rs](../../crates/tisvlay-world/src/systems/mod.rs)); once it's spent, `PendingConnect` is dropped ([crates/tisvlay-world/src/systems/facts.rs](../../crates/tisvlay-world/src/systems/facts.rs)). LG is re-armed by carrier, route and wake events; nothing watches for `/dev/cecN` reappearing. The same gap is a boot race: at this boot the adapter registered 3 s after the daemon started, inside the budget, but a slower inputattach would leave CEC down until a manual `reconnect` or the next wake.
- **No info-level CEC logs.** The journal shows nothing about a claim (address, name) or a failed connect; everything above came from kernel CEC debug and `cec-ctl`.
- **Empty All Device Types.** Our CEC 2.0 claims broadcast `<Report Features>` as `4f a6 06 00 00 00`: the All Device Types operand is 0, and `cec-ctl -d /dev/cec0` prints `All Device Types :` with nothing after it. linux-cec 0.2.1 sets `log_addr_type` and `primary_device_type` but never `all_device_types`; `cec-ctl --playback` fills it (`All Device Types : Playback`). linux-cec also has no setter for `cec_version` (the kernel's default is 2.0) and doesn't expose its fd, so neither can be fixed from our side without changing linux-cec. A 1.4 TV like the C3 ignores `<Report Features>`; a 2.0 TV might not.
- **Feature Abort for early queries.** Right after a claim the TV sends `04 8c` (Give Device Vendor ID) and `04 8f` (Give Device Power Status, three times). The kernel answers both with Feature Abort (`40 00 8c 00`, `40 00 8f 00`): we set no vendor ID, and the follower monitor that could see `8f` isn't open yet when the claim goes out. Naming still worked after the replug, so neither seems to affect the label.

## Open questions

- **Why the boot claim landed on LA 12.** The polls to 4 and 11 were acknowledged (no poll-failure lines), but neither a reclaim after a long detach (step 6) nor one 0.2 s after re-registration (step 7) reproduced it. Candidates: firmware state that only a full warm reboot produces, or the TV waking at the same time (LG came online after WoL at 09:52:18). A reboot with `options cec debug=2` would show who acknowledged.
- **Whether the tty can carry writes inside `pulse8_disconnect`.** When inputattach exits, the disconnect runs while the line discipline is closing. If writes fail there, a disarm-on-disconnect patch can't work, and the disarm has to happen before (e.g. the daemon clearing its claim on shutdown).

## Diagnostics

- **Kernel CEC debug:** `echo N | sudo tee /sys/module/cec/parameters/debug`. Level 0 still prints `polling for LA … failed`; level 1 adds the claim (`physical address: …, claim 1 logical addresses`, `config: la 4 pa 1.0.0.0`); level 2 adds every message (`cec_transmit_msg_fh`, `cec_transmit_done_ts: status 0x..`, `cec_received_msg_ts`). Status `0x01` is OK (ACKed), `0x04` NACK. At boot, set it with `boot.extraModprobeConfig = "options cec debug=2";` (the `cec` core is a module).
- **Adapter claim:** `cec-ctl -d /dev/cec0` (logical address, OSD name, vendor ID, CEC version, All Device Types). `cec-ctl -d /dev/cec0 -S` polls the bus and prints each device's identity plus the topology. `cec-ctl` comes from `v4l-utils` (`nix-shell -p v4l-utils`).
- **Driver parameters:** `/sys/module/pulse8_cec/parameters/{debug,persistent_config}`.
- **Reproducer (the detach experiment):** with the daemon running, `sudo systemctl stop pulse8-cec-inputattach@ttyACM0`, poll the SSAP label every 10 s for about 2 minutes (it flips to "LG DVD Player" after about 60 s), then `sudo systemctl start pulse8-cec-inputattach@ttyACM0` and `tisvlay reconnect <cec-uuid>` (the daemon doesn't redial by itself). An HDMI replug at the adapter restores the label afterwards.
