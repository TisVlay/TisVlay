# TV-driven PC power and input over HDMI-CEC

TisVlay drives the TV from the PC: PC events (boot, sleep, lock, idle, DPMS) become TV actions. HDMI-CEC also carries the reverse direction, and a Pulse-Eight adapter, the kernel's CEC core and the daemon's follower monitor already give it three openings: the TV's power key could wake a suspended PC, the TV going to standby could sleep or lock it, and the TV's remote could drive the desktop. Grouped here because they share a trigger (a user asks for "the TV controls the PC") and a design question: what TisVlay owns, what's host configuration, and what's left to the TV and the desktop.

Backend model: [docs/cec/protocol.md](../cec/protocol.md). Adapter lifecycle: [docs/watchers/devices.md](../watchers/devices.md). The Pulse-Eight's autonomous mode and its label trade-off: [docs/bugs/cec-pulse8-autonomous-label.md](../bugs/cec-pulse8-autonomous-label.md).

## What exists today

| Direction | Mechanism already there | Gap |
|---|---|---|
| TV wakes the PC | The Pulse-Eight firmware's autonomous mode, armed on every claim and kept armed through a suspend (#211 disarms only on disconnect) | Whether its wake reaches the PC (the adapter's USB wakeup is disabled here); no way to choose it; nothing for non-Pulse-Eight adapters |
| TV sleeps or locks the PC | The CEC connection's monitor turns the TV's own broadcasts (`<Standby>`, `<Report Power Status>`) into `state/power`, and the session service already mirrors `state/power` | No event or action consumes a TV-side power change; TisVlay can't tell the TV's own standby from one it caused |
| TV remote as input | The kernel's `rc-cec` device and keymap, registered when the adapter attaches | The kernel forwards keys only when the claim allows RC passthrough, which TisVlay's claim doesn't (and linux-cec can't yet set) |

## 1. The TV wakes the PC

Only the adapter is awake while the PC is suspended: TisVlay's processes are frozen, so this is firmware plus host power plumbing, and TisVlay's part is configuration and docs.

**The Pulse-Eight's side.** The driver's header notes say that in autonomous mode the dongle "is known to attempt wakeup" on `<User Control Pressed>` Power, Power On or Power Toggle, on `<Set Stream Path>` to its own physical address, and on LG's vendor command `0x03 0x00`. Upstream's design (`e28a6c8b3fcc`, 2016-08-22, Johan Fjeldtvedt) is exactly this: the claim's settings are written to the adapter so it keeps operating "when it is disconnected from the driver (typically by going into suspend)". A suspend doesn't disconnect the driver, so #211's disarm doesn't touch it; but the firmware only takes over once it misses the driver's pings (every 15 s; the label flipped about 60 s after a detach), so a wake attempt needs the PC to have been asleep that long.

**The host's side.** The dongle's wake has to travel as a USB remote wakeup: the device must advertise it, the kernel must have it enabled for that device, and the controller must be allowed to wake the system from the sleep state in use. Read on pox (2026-10-24):

| Item | Value | Meaning |
|---|---|---|
| Adapter `bmAttributes` (`/sys/bus/usb/devices/1-5`) | `a0` | Bit `0x20`: the adapter supports remote wakeup |
| Adapter `power/wakeup` | `disabled` | Capable, but not allowed to wake the system (the file exists only for wake-capable devices) |
| Root hub `usb1` `power/wakeup` | `disabled` | May need enabling too; part of the check below |
| xHCI controller `0000:00:14.0` `power/wakeup` | `enabled` | The controller may wake the system |
| `/proc/acpi/wakeup` | `XHCI S4 *enabled` | ACPI allows the controller to wake from states up to S4 |
| `/sys/power/mem_sleep` | `s2idle [deep]` | Suspend is S3 |

So by default a dongle wake can't reach pox: the adapter's own wakeup is off. With it enabled for Check B, pox did resume when the TV's power key was pressed, but three other devices on the same bus were also allowed to wake it, so the resume isn't attributed to the dongle yet (Check B result, and Check B′ to settle it).

**The wake trigger is already on the bus.** Check A showed the TV turning on with its remote broadcasts `<Set Stream Path>` to the input it lands on (`0f 86 10 00` for HDMI 1, the PC's adapter at 1.0.0.0). That's one of the messages the firmware notes say it wakes on, and it's a policy signal too: it names the input, so "wake the PC only when the TV comes up on the PC's input" falls out of it, where "wake on any TV power-on" would also wake the PC for the Chromecast on HDMI 4.

**The trade-off.** Keeping the dongle armed while suspended is what lets it wake the PC, and it's also what lets it answer the TV on its own with LG's vendor ID, which the bug doc showed pins "LG DVD Player" on the input until the TV re-detects the device. A suspend long enough for the firmware to take over may cost the label each time (Check B reads it after resume). The two can't be had together from the firmware as it is.

**The knob (decided 2026-10-24).** Whether the dongle stays armed while the PC sleeps is host configuration, never a TisVlay profile setting: it's a driver setting, Pulse-Eight-specific, and applying it needs root, which the sandboxed daemon doesn't have. The shape is a `pulse8-cec` module parameter that skips arming on claim, plus a NixOS option beside `cec.pulse8.enable` ([docs/plans/cec-pulse8-autonomous.md](../plans/cec-pulse8-autonomous.md) step 7). Enabling the adapter's USB wakeup is a second, independent switch (a udev rule on the adapter's USB ids setting `power/wakeup`), and "the TV's power key wakes the PC" needs both.

**Other adapters (unverified).** DisplayPort-AUX CEC on amdgpu or i915 is driven by the GPU driver over the DP AUX channel, so it should go quiet with the GPU at suspend, with no firmware of its own to answer the TV or raise a wake. Nothing here verified it; the path for those hosts would be another wake source (Wake-on-LAN from a phone, a USB keyboard), not CEC.

## 2. The TV sleeps or locks the PC

**The signal.** When the TV goes to standby it broadcasts `<Standby>` (`0f 36`) and then its power status as standby (`0f 90 01`); when it comes back it broadcasts `0f 90 00` and a `<Set Stream Path>` to the input it lands on (Check A). The CEC monitor already turns TV-originated broadcasts into `state/power` (it ignores other initiators). An LG over the network reports the same transitions through SSAP, so the event is transport-agnostic: "the TV changed power state, and TisVlay didn't ask it to". The wake side can carry more on CEC: whether the TV came up on this PC's input, from the Set Stream Path address.

**The event and the action.** Every power event maps to a configurable profile action, never a hardcoded one, so this is a pair of new events with a new action vocabulary: the existing `EventAction`s act on the TV, these act on the PC. A sketch: `on_tv_standby` and `on_tv_wake` in the **user** profile (the per-user behavior, assigned per TV), each taking a host action from a small set: nothing, lock the session, suspend. Defaults: nothing, so it's strictly opt-in.

**Who acts.** The session service: it runs in the user's session, already mirrors every TV's `state/power` (for the idle drift re-assert), and logind lets an active session lock itself and suspend the system without extra polkit rules. The daemon is a system user outside any session, so it would need a polkit rule to suspend and has no session to lock.

**The origin problem.** TisVlay itself turns TVs off: the system profile's sleep and shutdown actions, idle and lock actions (screen-off is emulated as power-off on CEC), a user's `power off`. The TV then broadcasts exactly the same pair as for its own remote, `0f 36` then `0f 90 01` (Check A against a traced power-off where it followed our `40 36` in the same second): nothing in the TV's broadcast says who asked. So a naive event would chain "idle → TV off → PC suspend", or fire during the PC's own sleep. The event needs to know the transition wasn't TisVlay's. Two options: the world stamps each power transition with its cause (it knows its last power command per TV, so `state/power` could carry "requested" versus "external"; wire-visible), or the session suppresses the event for a window after any power command it can see (simpler, but blind to the daemon's own system-profile dispatches, so it doesn't work alone). The stamp is the sound one.

**Hazards.** A TV that sleeps on its own timer (no-signal auto-off, eco settings) would also fire it, which is probably what a user who opts in wants, but it should be said. The unconfirmed C3 behavior of ignoring `<Standby>` just after waking ([docs/concerns/cec-standby-ignored-after-wake.md](../concerns/cec-standby-ignored-after-wake.md)) doesn't affect this direction.

## 3. The TV remote as input

**What's there (read on pox, 2026-10-24).** The kernel registered `rc0` (`rc-cec` keymap, driver `cec`, protocol `cec` enabled, input node `/dev/input/event26`, owned `root:input`), with no `wakeup_protocols`, so it can't be a wake source itself. The keymap covers navigation (`KEY_UP`…`KEY_OK`), digits, menus, media keys, volume and mute, and maps CEC's Power key (`0x40`) to **`KEY_POWER`**. On this desktop PowerDevil holds the power-key inhibitor, and its AC `PowerButtonAction` is `1`, which reads as sleep (assuming PowerDevil's action numbering; not verified). The Pulse-Eight's separate USB HID interface (`input11`, a mouse) is unrelated to CEC keys.

**Why no keys arrive today.** The kernel forwards `<User Control Pressed>` to `rc-cec` only when the claim sets `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`. TisVlay's claim doesn't: `cec-ctl -d /dev/cec0` shows `Logical Addresses : 1` with no flag, while a `cec-ctl --playback` claim shows `(Allow RC Passthrough)`. linux-cec 0.2.1 has no setter for log-address flags, so enabling it would need a linux-cec change, like the missing `all_device_types` and `cec_version` setters. The TV also only sends remote keys to the active source, and on an LG only when SIMPLINK is on and the PC's input is selected.

**Options.**
- **Stay out (today).** Document that remote keys don't reach the desktop under TisVlay, and why.
- **Opt-in passthrough.** A CEC vendor-config field that sets the flag on the claim (once linux-cec can), so the remote drives the desktop through the kernel's keymap with no TisVlay key handling. The `KEY_POWER` mapping is the hazard: a remote power press would suspend this PC. Defang it with a keymap override (an `rc_keymaps` file or a hwdb entry dropping `0x40`), shipped by the NixOS option and documented for other distros, rather than filtering keys in TisVlay.
- **TisVlay maps keys itself.** Read `<User Control Pressed>` from the follower monitor and act (volume to the PC, media keys over MPRIS). More control, much more surface; only if the kernel path proves wrong.

## 4. Install and configuration

- **NixOS.** Options beside `cec.enable` and `cec.pulse8.enable`, names tentative: one to keep the dongle armed while suspended (the module parameter above), one to enable the adapter's USB wakeup (a udev rule on `2548:1001`/`2548:1002`), and one for remote passthrough with its `KEY_POWER` override. Each is opt-in and says what it trades.
- **Other distros.** The same three as documented snippets in [docs/ideas/cec-distro-setup.md](cec-distro-setup.md)'s helper or package: a modprobe option, a udev rule, a keymap file.
- **TisVlay itself.** Only the TV→PC events are TisVlay configuration (user-profile fields, editable in the GUI's profile pane like the others). The GUI could show, read-only, whether the adapter can wake this PC (its `power/wakeup`), so a user can see why "the TV wakes the PC" doesn't.
- **User guide** (held until the CEC follow-ups near their end): what "the TV controls the PC" can and can't do per adapter type, the label trade-off in plain words, and the NixOS options.

## Checks to run

The TV is pox's only display, so any check with the TV dark runs as one unattended block: run `sudo -v` first, start the block, follow the instruction it prints at the start, and don't touch anything else until `DONE` prints.

**A. What the TV says when its own remote turns it off and on.** TV on, PC awake. Start the block, then turn the TV off with its remote at once, wait about 20 s, and turn it on again.

```fish
sudo -v
begin
    echo 2 | sudo tee /sys/module/cec/parameters/debug >/dev/null
    date +%T; echo "turn the TV OFF with its remote now, then ON again in about 20 s"
    sleep 45
    journalctl -k --since '-50s' --no-pager | grep -E 'cec_(received_msg_ts|transmit_msg_fh)'
    tisvlay --json list | jq -c '.[] | {n: ."meta/identity".name, s: ."state/power".state}'
    echo 0 | sudo tee /sys/module/cec/parameters/debug >/dev/null
    echo DONE
end 2>&1 | tee ~/cec-tv-power-keys.log
```

- `0f 36` from the TV after the off press: the TV announces its own standby, so section 2's signal exists. No `0f 36`: TisVlay only learns of it by polling, and section 2 needs a power query on a timer.
- On the on press, look for `0f 86 10 00` (Set Stream Path to 1.0.0.0), `0f 85` (Request Active Source) or a message to us (`04 …`): those are what a suspended dongle could wake on. Only `0f 87` / `0f 84` traffic means the TV announces itself but asks nothing of the PC.

**Result (2026-10-24, 15:36, LG C3).** Off with the remote: `0f 36` then `0f 90 01`, the same pair as when TisVlay sends `<Standby>`, so section 2's signal exists and the cause stamp is required, not optional. On with the remote, 8 s later: `0f 90 00`, then `0f 86 10 00`, Set Stream Path to the PC's adapter (the TV returned to its last input, HDMI 1), then a poll of every logical address and a `<Give Device Vendor ID>` to us (`04 8c`, which we answer with Feature Abort). The Chromecast woke with the TV. No `<User Control Pressed>` reached us for either key press: the TV doesn't send its power key over CEC, and nothing else arrives without passthrough (section 3).

**B. Does the dongle wake pox from suspend?** Enables the adapter's USB wakeup for the test and restores it after. Start the block, wait until the TV has been dark for about 90 s (the firmware takes over only after it misses the driver's pings), then press the TV remote's power key, not the PC's. If the PC doesn't resume within a minute, wake it with its own keyboard; the log still records the outcome.

```fish
sudo -v
begin
    set before (cat /sys/bus/usb/devices/1-5/power/wakeup)
    echo enabled | sudo tee /sys/bus/usb/devices/1-5/power/wakeup >/dev/null
    date +%T; echo "suspending in 5 s; wait ~90 s after the TV goes dark, then press the TV remote's power key"
    sleep 5
    systemctl suspend
    sleep 30
    date +%T
    journalctl -b --since '-15min' --no-pager -o short-precise | grep -E 'PM: suspend (entry|exit)|usb 1-5|xhci' | tail -20
    cat /sys/power/pm_wakeup_irq 2>&1; grep -E 'xhci' /proc/interrupts | head -2
    echo $before | sudo tee /sys/bus/usb/devices/1-5/power/wakeup >/dev/null
    tisvlay --json list | jq -r '.["2d64d464-1aa3-4065-bdf4-718709df398f"]."state/input".available[] | select(.id=="HDMI_1") | .label'
    echo DONE
end 2>&1 | tee ~/cec-dongle-wake.log
```

- `PM: suspend exit` within seconds of the TV press: the dongle may have woken pox; Check B′ attributes it. If it doesn't resume, rerun with the root hub enabled too (`/sys/bus/usb/devices/usb1/power/wakeup`) before concluding.
- No resume until the PC's own key: the firmware doesn't raise a USB wake, or the host blocks it; section 1 needs another route or stays out.
- The last line is the TV's label for the input after the suspend: "LG DVD Player" means the armed dongle pinned it while the PC slept, the trade-off in numbers.

**Result (2026-10-24, 15:41, LG C3, Pulse-Eight).** Suspend entry 15:41:05 (S3), resume 15:42:50, about 105 s later, matching the TV power-key press after the 90 s wait. The kernel recorded no wake IRQ (`pm_wakeup_irq` reads "No data available" here, so it can't attribute a wake on this host). The controller the adapter sits on (`0000:00:14.0`, bus 1) resumed with `USBSTS 0x411`, whose `0x10` is Port Change Detect, the trace a device's remote wakeup leaves; the other controller resumed with `0x401`, no port change. So a device on bus 1 woke pox. But three other bus-1 devices also had wakeup enabled (two USB receivers at `1-2` and `1-7`, and the PiKVM composite device at `1-6`), and any of them leaves the same trace, so the dongle isn't proven: Check B′ isolates it. The input's label read "TisVlay" after resume; whether it changed during the suspend wasn't observed.

**B′. Does the dongle wake pox, with nothing else allowed to?** As B, but the other bus-1 devices that may wake the system have their wakeup turned off for the test and restored after. The ids are this host's (from `power/wakeup` = `enabled` on bus 1); list them again on another host. With the keyboard and mouse receivers unable to wake it, wake pox with its power button if the TV's key doesn't within a minute.

```fish
sudo -v
begin
    set others 1-2 1-6 1-7
    set saved
    for d in $others
        set -a saved (cat /sys/bus/usb/devices/$d/power/wakeup)
        echo disabled | sudo tee /sys/bus/usb/devices/$d/power/wakeup >/dev/null
    end
    set before (cat /sys/bus/usb/devices/1-5/power/wakeup)
    echo enabled | sudo tee /sys/bus/usb/devices/1-5/power/wakeup >/dev/null
    date +%T; echo "suspending in 5 s; wait ~90 s after the TV goes dark, then press only the TV remote's power key"
    sleep 5
    systemctl suspend
    sleep 30
    date +%T
    journalctl -b --since '-10min' --no-pager -o short-precise | grep -E 'PM: suspend (entry|exit)|USBSTS'
    echo $before | sudo tee /sys/bus/usb/devices/1-5/power/wakeup >/dev/null
    for i in (seq (count $others))
        echo $saved[$i] | sudo tee /sys/bus/usb/devices/$others[$i]/power/wakeup >/dev/null
    end
    tisvlay --json list | jq -r '.["2d64d464-1aa3-4065-bdf4-718709df398f"]."state/input".available[] | select(.id=="HDMI_1") | .label'
    echo DONE
end 2>&1 | tee ~/cec-dongle-wake-isolated.log
```

- Resume within seconds of the TV press, with Port Change Detect (`0x10`) on bus 1's `USBSTS`: the dongle wakes pox, and section 1 is configuration only.
- No resume until the power button: one of the other devices woke pox in Check B, and the dongle's "attempt wakeup" doesn't reach the host as configured.

**C. Do the TV remote's keys reach the desktop with RC passthrough?** TV on and showing the PC. This replaces the daemon's claim with `cec-ctl`'s, which allows passthrough, for 30 s. Don't press the remote's power key: `KEY_POWER` would suspend this PC.

```fish
sudo systemctl stop tisvlay-daemon
nix-shell -p v4l-utils --run 'cec-ctl -d /dev/cec0 --playback --osd-name TisVlay' | grep -E 'Logical Addresses  '
echo "press arrows and OK on the TV remote for 30 s"
nix-shell -p evtest --run 'timeout 30 evtest /dev/input/event26'
sudo systemctl start tisvlay-daemon
```

- Key events for each press: the kernel path works, and passthrough is a linux-cec flag away. Nothing: the TV doesn't forward keys to this input (check SIMPLINK is on and the PC's input is selected), and section 3 stays "document and stay out".

## Open questions

- **Suspend wake against the label.** Should the TV's power key be able to wake the PC, keeping the dongle armed while suspended and accepting a possibly pinned label after each long suspend, or should a suspended PC disarm it too? This decides the module parameter's default in the NixOS option. One data point so far: after Check B's 105 s suspend the label still read "TisVlay".
- **TV-to-PC sleep.** Is it wanted at all, as opt-in user-profile events? And is the world-side cause stamp on `state/power` (a wire change) acceptable as its basis?
- **Remote passthrough.** Stay out, or pursue the linux-cec flag setter and an opt-in with a `KEY_POWER` override?
- **Surfacing.** Should the GUI show whether the adapter can wake the PC?

## Trigger

A user asks for the TV to wake, sleep or lock the PC, or to drive the desktop with its remote; or Checks A to C run and settle which of the sections are viable.
