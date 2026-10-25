# TV-driven PC power and input

TisVlay drives the TV from the PC: PC events (boot, sleep, lock, idle, DPMS) become TV actions. The reverse direction has openings on every backend and more on HDMI-CEC:
- **Any backend:** the TV going to standby or waking can sleep or lock the PC. CEC reports it in broadcasts, LG over the network through its power-state subscription.
- **CEC only:** the TV's power key can wake a suspended PC, through a Pulse-Eight adapter's firmware; and the TV's remote can drive the desktop, through the kernel's CEC core.

They're grouped here because they share a trigger (a user asks for "the TV controls the PC") and a design question: what TisVlay owns, what's host configuration, and what's left to the TV and the desktop. The decided parts are planned in [docs/plans/tv-controls-pc.md](../plans/tv-controls-pc.md), as a transport-agnostic core plus CEC extras. Most of this doc was written from the CEC investigation; [Not yet planned](#not-yet-planned) tracks what generalizes further.

Backend model: [docs/cec/protocol.md](../cec/protocol.md). Adapter lifecycle: [docs/watchers/devices.md](../watchers/devices.md). The Pulse-Eight's autonomous mode and its label trade-off: [docs/bugs/cec-pulse8-autonomous-label.md](../bugs/cec-pulse8-autonomous-label.md).

## What exists today

| Direction | Mechanism already there | Gap |
|---|---|---|
| TV wakes the PC | The Pulse-Eight firmware's autonomous mode, armed on every claim and kept armed through a suspend (#211 disarms only on disconnect) | Whether its wake reaches the PC (the adapter's USB wakeup is disabled here); no way to choose it; nothing for non-Pulse-Eight adapters |
| TV sleeps or locks the PC | The CEC connection's monitor turns the TV's own broadcasts (`<Standby>`, `<Report Power Status>`) into `state/power`, and the session service already mirrors `state/power` | No event or action consumes a TV-side power change; TisVlay can't tell the TV's own standby from one it caused |
| TV remote as input | The kernel's `rc-cec` device and keymap, registered when the adapter attaches | The kernel forwards keys only when the claim allows RC passthrough, which TisVlay's claim doesn't (and linux-cec can't yet set) |

## Decisions (2026-10-24, placement revised 2026-10-25)

Taken after Checks A to C ran. The rest of this doc keeps the reasoning; where a section lists options, these are the ones chosen.

- **Where each setting lives (2026-10-25).** Two questions place it. Is it a fact about the setup or a choice about behavior? Facts go in device config, choices in a profile. For a choice, whose scope does the action touch? The machine's (suspend, the TV waking the PC, what the shared adapter forwards) goes in the system profile, run by the daemon. The user's session (lock) goes in the user profile, run by the session service. What a backend can honor is a capability it declares (`REMOTE_POWER`, `REMOTE_KEYS`, `WAKE_HOST`), so the profile fields stay backend-neutral and do nothing where the TV can't honor them, the way `restore_input` needs `INPUT`. Each profile gets its own host-action type, so a user profile can't express a suspend. The table is in [the plan](../plans/tv-controls-pc.md#where-the-settings-live).
- **The TV wakes the PC: configurable at runtime, from the start.** "The TV may wake this PC" is behavior policy, so a system-profile field, editable in the GUI like the others. It's applied right before each sleep, when logind's `PrepareForSleep` fires (the daemon already watches it): the kernel reads the adapter's `power/wakeup` at suspend time. The daemon can't write sysfs itself (the packaged unit's `ProtectKernelTunables=true` makes `/sys` read-only, and the attribute is root's), so it goes through a small privileged helper: a root oneshot unit that applies a setting the daemon writes, which the daemon may start through a narrow polkit rule allowing only that unit (no prompt, so it works at sleep time), installed by the NixOS module and the distro packages. Off by default: a TV turned on for something else would otherwise wake the PC, and the label across long armed suspends is still unmeasured. The same hop can later carry the Pulse-Eight's disarm-on-suspend module parameter, also root-only. Whether the adapter can wake the PC at all is its `WAKE_HOST` capability.
- **The TV sleeps or locks the PC: opt-in profile events, split by scope.** `on_tv_standby` and `on_tv_wake`, defaulting to nothing, on the world-side cause stamp on `state/power` (a wire change) so TisVlay's own power-offs never fire them. Lock is in the user profile, run by the session service. Suspend is in the system profile, run by the daemon through logind with a polkit rule, so it works with nobody logged in and a shared machine doesn't depend on one user's settings. Setting both runs both.
- **The remote: TisVlay gates, the desktop maps.** A per-TV system-profile toggle, "send the remote's keys to the desktop", applied by the daemon at claim (it needs the linux-cec setter for the passthrough flag) where the adapter has `REMOTE_KEYS`. The kernel keymap and the desktop's own shortcuts decide what keys mean: no per-key table in TisVlay, which would duplicate input-remapper, keyd, hwdb and the desktop's shortcuts and need input injection for navigation. If a need appears, a small set of TisVlay *intents* fits better than raw keys.
- **The remote's power key: its own profile event.** The keymap drops scan `0x40` (static host configuration), so the kernel never turns SIMPLINK's device power-off into `KEY_POWER`. The daemon's follower monitor receives the message instead (the kernel queues `<User Control Pressed>` to followers whether or not passthrough is on: in `cec_receive_notify` its passthrough case only adds the `rc_keydown` before falling through to the follower queue; kernel 7.3 source), and it becomes `on_remote_power`, split like TV standby: lock in the user profile, suspend in the system profile. It's separate from TV standby because it's explicit and aimed at the PC: it never fires on its own timer and never echoes TisVlay's commands, so it needs no cause stamp. Turning the TV off isn't one of its choices: a suspend already runs the system profile's `on_sleep` and a lock the user profile's `on_session_lock`, so the TV follows the PC through the events it already has.

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

**The event and the action.** Every power event maps to a configurable profile action, never a hardcoded one, so this is a pair of new events with a new action vocabulary: the existing `EventAction`s act on the TV, these act on the PC. `on_tv_standby` and `on_tv_wake` exist in both profiles, each taking its scope's host action: lock the session in the user profile, suspend in the system profile, or nothing. Defaults: nothing, so it's strictly opt-in.

**Who acts.** Each scope's own service. The session service locks: it runs in the user's session and already mirrors every TV's `state/power` (for the idle drift re-assert). The daemon suspends: it's a system user outside any session, so it needs a polkit rule for logind's `Suspend`, but it runs at the login screen and with nobody logged in, where no session service does. The first sketch put both in the user profile, since logind lets an active session suspend the system without extra rules; that left suspend to one user's settings on a shared machine, and to nothing when nobody's logged in.

**The origin problem.** TisVlay itself turns TVs off: the system profile's sleep and shutdown actions, idle and lock actions (screen-off is emulated as power-off on CEC), a user's `power off`. The TV then broadcasts exactly the same pair as for its own remote, `0f 36` then `0f 90 01` (Check A against a traced power-off where it followed our `40 36` in the same second): nothing in the TV's broadcast says who asked. So a naive event would chain "idle → TV off → PC suspend", or fire during the PC's own sleep. The event needs to know the transition wasn't TisVlay's. Two options: the world stamps each power transition with its cause (it knows its last power command per TV, so `state/power` could carry "requested" versus "external"; wire-visible), or the session suppresses the event for a window after any power command it can see (simpler, but blind to the daemon's own system-profile dispatches, so it doesn't work alone). The stamp is the sound one.

**Hazards.** A TV that sleeps on its own timer (no-signal auto-off, eco settings) would also fire it, which is probably what a user who opts in wants, but it should be said. The unconfirmed C3 behavior of ignoring `<Standby>` just after waking ([docs/concerns/cec-standby-ignored-after-wake.md](../concerns/cec-standby-ignored-after-wake.md)) doesn't affect this direction.

## 3. The TV remote as input

**What's there (read on pox, 2026-10-24).** The kernel registered `rc0` (`rc-cec` keymap, driver `cec`, protocol `cec` enabled, input node `/dev/input/event26`, owned `root:input`), with no `wakeup_protocols`, so it can't be a wake source itself. The keymap covers navigation (`KEY_UP`…`KEY_OK`), digits, menus, media keys, volume and mute, and maps CEC's Power key (`0x40`) to **`KEY_POWER`**. On this desktop PowerDevil holds the power-key inhibitor, and its AC `PowerButtonAction` is `1`, which reads as sleep (assuming PowerDevil's action numbering; not verified). The Pulse-Eight's separate USB HID interface (`input11`, a mouse) is unrelated to CEC keys.

**Why no keys arrive today.** The kernel forwards `<User Control Pressed>` to `rc-cec` only when the claim sets `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`. TisVlay's claim doesn't: `cec-ctl -d /dev/cec0` shows `Logical Addresses : 1` with no flag, while a `cec-ctl --playback` claim shows `(Allow RC Passthrough)`. linux-cec 0.2.1 has no setter for log-address flags, so enabling it would need a linux-cec change, like the missing `all_device_types` and `cec_version` setters. The TV also only sends remote keys to the active source, and on an LG only when SIMPLINK is on and the PC's input is selected.

**Options.**
- **Stay out (today).** Document that remote keys don't reach the desktop under TisVlay, and why.
- **Opt-in passthrough.** A per-TV setting that sets the flag on the claim (once linux-cec can), so the remote drives the desktop through the kernel's keymap with no TisVlay key handling. The `KEY_POWER` mapping is the hazard: a remote power press would suspend this PC. Defang it with a keymap override (an `rc_keymaps` file or a hwdb entry dropping `0x40`), shipped by the NixOS option and documented for other distros, rather than filtering keys in TisVlay.
- **TisVlay maps keys itself.** Read `<User Control Pressed>` from the follower monitor and act (volume to the PC, media keys over MPRIS). More control, much more surface; only if the kernel path proves wrong.

## 4. Install and configuration

- **NixOS.** Options beside `cec.enable` and `cec.pulse8.enable`, names tentative: one to keep the dongle armed while suspended (the module parameter above), one to enable the adapter's USB wakeup (a udev rule on `2548:1001`/`2548:1002`), and one for remote passthrough with its `KEY_POWER` override. Each is opt-in and says what it trades.
- **Other distros.** The same three as documented snippets in [docs/ideas/cec-distro-setup.md](cec-distro-setup.md)'s helper or package: a modprobe option, a udev rule, a keymap file.
- **TisVlay itself.** Profile fields, editable in the GUI's profile panes like the others: lock in the user profile; suspend, the TV waking the PC and the remote's keys in the system profile. The TV pane shows the host capabilities read-only (`WAKE_HOST` from the adapter's `power/wakeup`), so a user can see why "the TV wakes the PC" doesn't.
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

**Result (2026-10-24, 15:41, LG C3, Pulse-Eight).** Suspend entry 15:41:05 (S3), resume 15:42:50, about 105 s later, matching the TV power-key press after the 90 s wait. The kernel recorded no wake IRQ (`pm_wakeup_irq` reads "No data available" here, so it can't attribute a wake on this host). The controller the adapter sits on (`0000:00:14.0`, bus 1) resumed with `USBSTS 0x411` (Port Change Detect set), which first read as a device's remote wakeup; Check B′ showed the same value on every resume here, so it marks the controller's reinit, not the waker. Three other bus-1 devices also had wakeup enabled (two USB receivers at `1-2` and `1-7`, and the PiKVM composite device at `1-6`), so this run doesn't attribute the wake: Check B′ isolates the dongle. The input's label read "TisVlay" after resume; whether it changed during the suspend wasn't observed.

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

- Resume within seconds of the TV press: the dongle wakes pox, and section 1 is configuration only. (Bus 1's `USBSTS` shows Port Change Detect on every resume here, so it doesn't attribute the wake; the timing and the disabled alternatives do.)
- No resume until the power button: one of the other devices woke pox in Check B, and the dongle's "attempt wakeup" doesn't reach the host as configured.

**Result (2026-10-24, 16:01, first run).** pox resumed at 16:01:39, 33 s after suspend entry, seconds after the TV remote's power key was pressed (earlier than the block asked), with every other bus-1 USB wake source disabled. So the TV's power-on reaches pox as a USB remote wakeup through the dongle. Two caveats shape what it proves:

- **The dongle was undriven and disarmed.** `inputattach` had exited at Check B's resume (15:42, the bug fixed in #212's `88a3d854`) and nothing restarted it, so there was no `/dev/cec0`, and the patched driver had turned autonomous mode off on that disconnect. The dongle still woke pox, so the wake on the TV's Set Stream Path doesn't need autonomous mode armed. If that holds for a dongle driven up to the suspend, section 1's trade-off is a false choice: the TV can wake the PC with the dongle disarmed, and the label is safe.
- **It had been unattended for 18 minutes**, so the firmware's takeover delay (it acts only after missing the driver's 15 s pings) didn't apply. The 90 s wait in the block is for a dongle driven up to the suspend.

Port Change Detect (`USBSTS 0x411`) showed on bus 1 again, as it did in Check B and on every resume here, so it marks the controller's reinit after S3 rather than which device woke it; it isn't evidence on its own.

The input's label read "CEC Adapter" after this run, the disarmed dongle's factory identity (the patched detach test showed the same); it doesn't pin, and the next claim restores the configured name.

**Result (16:06, second run).** Repeated by accident with the dongle still undriven (`/dev/cec0` absent before the suspend): the TV power key woke pox again, 41 s after suspend entry, with the other wake sources disabled. Same conclusion as the first run.

**Result (16:11, driven).** `inputattach` started and CEC `connected` before the suspend (the block refuses to suspend otherwise). Suspend entry 16:11:10, resume 16:11:58 on the TV power key, other wake sources disabled. So **the TV's power key wakes pox through the dongle whether it's driven up to the suspend (armed) or not**, and section 1 is configuration: the adapter's USB wakeup, nothing in TisVlay. At resume `inputattach` exited cleanly and systemd restarted it 2 s later (#212's `88a3d854`), `/dev/cec0` came back, CEC reconnected on its own, and the label read "TisVlay". The suspend was 48 s, likely shorter than the firmware's takeover, so it says nothing yet about the label across a long suspend.

**C. Do the TV remote's keys reach the desktop with RC passthrough?** TV on and showing the PC. This replaces the daemon's claim with `cec-ctl`'s, which allows passthrough, for 30 s. Don't press the remote's power key: `KEY_POWER` would suspend this PC. The adapter's `rc-cec` input gets a new event number each time the adapter re-attaches, so the block looks it up instead of hardcoding it, and it stops early if the adapter isn't attached.

```fish
begin
    if not test -e /dev/cec0
        echo "no /dev/cec0: run 'sudo systemctl start pulse8-cec-inputattach@ttyACM0' first"
    else
        set ev (path basename /sys/class/rc/rc*/input*/event*)[1]
        echo "rc-cec input: /dev/input/$ev"
        sudo systemctl stop tisvlay-daemon
        nix-shell -p v4l-utils --run 'cec-ctl -d /dev/cec0 --playback --osd-name TisVlay' | grep -E 'Logical Addresses  '
        echo "press arrows and OK on the TV remote for 30 s"
        nix-shell -p evtest --run "timeout 30 evtest /dev/input/$ev"
        sudo systemctl start tisvlay-daemon
    end
end
```

- Key events for each press: the kernel path works, and passthrough is a linux-cec flag away. Nothing: the TV doesn't forward keys to this input (check SIMPLINK is on and the PC's input is selected), and section 3 stays "document and stay out".

**Result (2026-10-24, 16:19, LG C3).** With `cec-ctl`'s passthrough claim (`Logical Addresses : 1 (Allow RC Passthrough)`), the remote drove the desktop: the arrows, OK, Exit, CH+/−, the four color keys, rewind, play, pause, fast-forward, stop, Info and Home (`KEY_ROOT_MENU`) each arrived as a key press and release on the `rc-cec` input, and the arrows also reached the focused terminal as keystrokes. The TV's own power key isn't forwarded, but SIMPLINK's device power-off is: it arrived as `KEY_POWER` (scan `0x40`) at 16:20:25, and pox entered suspend in the same second, so KDE's power-button action here is sleep. That's the hazard section 3 names, confirmed: passthrough without a `KEY_POWER` override lets anyone with the remote suspend the PC through SIMPLINK. pox resumed at 16:21:13, `inputattach` restarted after the resume (#212's fix, second time), and the daemon reclaimed without passthrough.

## Not yet planned

Raised while generalizing beyond CEC (2026-10-24). Not mature enough for the plan; picked up when the planned core lands or a user asks.

- **Input-change events.** Both backends report which input the TV shows: CEC through `<Set Stream Path>` and `<Active Source>` (Check A saw `0f 86 10 00` at power-on), LG through its foreground-app subscription, which the world already reads for `primary_input` and `automate_when: on_primary`. That gives `on_tv_left_pc` (switched to the Chromecast: lock, pause media, blank) and `on_tv_returned_to_pc` (wake the screen), with no new data source.
- **Screen-off events.** LG reports its own screen-off mode (`Screen Off`, the OLED screen-off from its menu or remote) apart from standby; "screen off → lock the PC" is a natural pairing. CEC has no screen-off state of its own (the backend emulates screen as power).
- **More than one TV.** With two TVs on one PC (the C3 and the G4 here: sometimes both show pox, more often one at a time), one TV's standby shouldn't suspend or lock the PC while the other is in use. The plan has no guard (decided 2026-10-25): each TV acts alone, and per-TV profile assignment decides which TVs' standby acts at all. A guard would ask whether another TV still shows the PC, with these questions open:
  - **Which TVs block.** Any TV that's on? One that's on and on its `primary_input`, the test `automate_when: on_primary` uses? LG knows its input, but CEC reports none, so a CEC TV that's on would have to count as showing the PC: it fails safe, the reverse of `automate_when`'s benefit of the doubt. Or only TVs whose profile sets the same host action ("when the last of them goes off"), or only the TV the EDID probe matches to the watched connector?
  - **Whether it covers lock too.** The G4 going to standby shouldn't lock the session in use on the C3 either, though a lock is cheaper to undo than a suspend.
  - **Where it runs.** The daemon's world sees every TV, and the session service mirrors every TV's `state/power`, so either scope can apply it.
- **No unlock, and remote sessions.** Host actions should never include unlocking: anyone with the TV's remote could open the PC. And while a remote session arms a TV (Sunshine and the like), its standby mustn't suspend the PC under the remote user; the events would honor the existing `while_remote` gate like every other dispatch. The world already mirrors the armed set, so the daemon's suspend can honor it as well as the session's lock.
- **Behavior knobs in CEC's device config.** Two fields predate the placement rule and are choices, not facts: `emulate_screen_via_power` (dispatch screen actions as power) and `advertise_osd_name` (whether to set the OSD name at all). The rule's shape for them is a capability plus a profile choice, or a split where the fact is the backend's and the choice the profile's. Moving them is a breaking config change, so it waits for the capability work here or a user hitting the gap. [cec-followups.md](cec-followups.md)'s emulated-screen toggle is the related client-side polish. `osd_name` is a fact (what this PC is called on the bus) and stays.
- **LG remote keys and pointer, without CEC.** Researched 2026-10-24: stock SSAP has no way to *receive* the remote's keys or pointer. The libraries ([lgtv2](https://github.com/hobbyquaker/lgtv2), [Home Assistant's webostv](https://www.home-assistant.io/actions/webostv.button/)) only send them, and the "pointer input socket" drives the TV's own pointer. The webOS homebrew docs list no key or remote service, and non-system apps reach only public Luna endpoints ([webOS Homebrew: Luna service bus](https://www.webosbrew.org/pages/luna-service-bus)). Receiving the Magic Remote takes an app running on the TV. [MagicRemoteService](https://github.com/Cathwyler/MagicRemoteService) (GPL-3.0, Windows PC side) installs a webOS app in developer mode, captures the remote through webOS DOM events, and forwards them over an unencrypted WebSocket. It maps clicks to gestures: a short OK press for a left click, a long press or the yellow key for a right click, and the scroll wheel forwarded. For TisVlay that would be a new component (a webOS app) with real costs: developer mode and its renewals, a re-install after most TV updates, and transport security to add. Low priority; CEC already carries the keys (not the pointer) where an adapter exists.

## Open questions

- **Suspend wake against the label.** Should the TV's power key be able to wake the PC, keeping the dongle armed while suspended and accepting a possibly pinned label after each long suspend, or should a suspended PC disarm it too? This decides the module parameter's default in the NixOS option. The wake doesn't need the dongle armed (Check B′: it woke pox undriven and disarmed), so disarming on suspend may not cost the wake; what's left is whether a long suspend with the dongle armed pins the label. So far: "TisVlay" after a 105 s and a 48 s suspend.

## Trigger

A user asks for the TV to wake, sleep or lock the PC, or to drive the desktop with its remote; or Checks A to C run and settle which of the sections are viable.
