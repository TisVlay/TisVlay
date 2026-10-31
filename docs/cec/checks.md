# CEC hardware checks: the TV acting on the PC

The checks that decided how TisVlay lets the TV act on the PC, run 2026-10-24 on an LG C3 with a Pulse-Eight USB-CEC adapter, before the feature was built. The test PC's only display was the TV, so each check with the TV dark ran as one unattended block. What they established is in [protocol.md](protocol.md#the-tv-acting-on-the-pc); the record stays here for its method and raw results. Check B′'s reading of an "undriven and disarmed" wake was later corrected: the driver's disarm clears only the adapter's working state, and the wake follows the autonomous flag in its EEPROM ([protocol.md](protocol.md#the-tv-waking-the-pc)). The records' "section" numbers are the original idea doc's: 1, the TV waking the PC; 2, the TV sleeping or locking the PC; 3, the remote as input.

The TV is the test PC's only display, so any check with the TV dark runs as one unattended block: run `sudo -v` first, start the block, follow the instruction it prints at the start, and don't touch anything else until `DONE` prints.

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

**B. Does the dongle wake the test PC from suspend?** Enables the adapter's USB wakeup for the test and restores it after. Start the block, wait until the TV has been dark for about 90 s (the firmware takes over only after it misses the driver's pings), then press the TV remote's power key, not the PC's. If the PC doesn't resume within a minute, wake it with its own keyboard; the log still records the outcome.

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

- `PM: suspend exit` within seconds of the TV press: the dongle may have woken the test PC; Check B′ attributes it. If it doesn't resume, rerun with the root hub enabled too (`/sys/bus/usb/devices/usb1/power/wakeup`) before concluding.
- No resume until the PC's own key: the firmware doesn't raise a USB wake, or the host blocks it; section 1 needs another route or stays out.
- The last line is the TV's label for the input after the suspend: "LG DVD Player" means the armed dongle pinned it while the PC slept, the trade-off in numbers.

**Result (2026-10-24, 15:41, LG C3, Pulse-Eight).** Suspend entry 15:41:05 (S3), resume 15:42:50, about 105 s later, matching the TV power-key press after the 90 s wait. The kernel recorded no wake IRQ (`pm_wakeup_irq` reads "No data available" here, so it can't attribute a wake on this host). The controller the adapter sits on (`0000:00:14.0`, bus 1) resumed with `USBSTS 0x411` (Port Change Detect set), which first read as a device's remote wakeup; Check B′ showed the same value on every resume here, so it marks the controller's reinit, not the waker. Three other bus-1 devices also had wakeup enabled (two USB receivers at `1-2` and `1-7`, and the PiKVM composite device at `1-6`), so this run doesn't attribute the wake: Check B′ isolates the dongle. The input's label read "TisVlay" after resume; whether it changed during the suspend wasn't observed.

**B′. Does the dongle wake the test PC, with nothing else allowed to?** As B, but the other bus-1 devices that may wake the system have their wakeup turned off for the test and restored after. The ids are this host's (from `power/wakeup` = `enabled` on bus 1); list them again on another host. With the keyboard and mouse receivers unable to wake it, wake the test PC with its power button if the TV's key doesn't within a minute.

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

- Resume within seconds of the TV press: the dongle wakes the test PC, and the wake is configuration only. (Bus 1's `USBSTS` shows Port Change Detect on every resume here, so it doesn't attribute the wake; the timing and the disabled alternatives do.)
- No resume until the power button: one of the other devices woke the test PC in Check B, and the dongle's "attempt wakeup" doesn't reach the host as configured.

**Result (2026-10-24, 16:01, first run).** the test PC resumed at 16:01:39, 33 s after suspend entry, seconds after the TV remote's power key was pressed (earlier than the block asked), with every other bus-1 USB wake source disabled. So the TV's power-on reaches the test PC as a USB remote wakeup through the dongle. Two caveats shape what it proves:

- **The dongle was undriven and disarmed.** `inputattach` had exited at Check B's resume (15:42, the bug fixed in #212's `88a3d854`) and nothing restarted it, so there was no `/dev/cec0`, and the patched driver had turned autonomous mode off on that disconnect. The dongle still woke the test PC, so the wake on the TV's Set Stream Path doesn't need autonomous mode armed. If that holds for a dongle driven up to the suspend, the wake-against-label trade-off is a false choice: the TV can wake the PC with the dongle disarmed, and the label is safe. (Correction, 2026-10-31: the disarm clears only the adapter's working state; its EEPROM still had autonomous mode on, which is what decides the wake, so this conclusion doesn't hold. See [protocol.md](protocol.md#the-tv-waking-the-pc).)
- **It had been unattended for 18 minutes**, so the firmware's takeover delay (it acts only after missing the driver's 15 s pings) didn't apply. The 90 s wait in the block is for a dongle driven up to the suspend.

Port Change Detect (`USBSTS 0x411`) showed on bus 1 again, as it did in Check B and on every resume here, so it marks the controller's reinit after S3 rather than which device woke it; it isn't evidence on its own.

The input's label read "CEC Adapter" after this run, the disarmed dongle's factory identity (the patched detach test showed the same); it doesn't pin, and the next claim restores the configured name.

**Result (16:06, second run).** Repeated by accident with the dongle still undriven (`/dev/cec0` absent before the suspend): the TV power key woke the test PC again, 41 s after suspend entry, with the other wake sources disabled. Same conclusion as the first run.

**Result (16:11, driven).** `inputattach` started and CEC `connected` before the suspend (the block refuses to suspend otherwise). Suspend entry 16:11:10, resume 16:11:58 on the TV power key, other wake sources disabled. So **the TV's power key wakes the test PC through the dongle whether it's driven up to the suspend (armed) or not**, and the wake is configuration: the adapter's USB wakeup, nothing in TisVlay. At resume `inputattach` exited cleanly and systemd restarted it 2 s later (#212's `88a3d854`), `/dev/cec0` came back, CEC reconnected on its own, and the label read "TisVlay". The suspend was 48 s, likely shorter than the firmware's takeover, so it says nothing yet about the label across a long suspend.

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

**Result (2026-10-24, 16:19, LG C3).** With `cec-ctl`'s passthrough claim (`Logical Addresses : 1 (Allow RC Passthrough)`), the remote drove the desktop: the arrows, OK, Exit, CH+/−, the four color keys, rewind, play, pause, fast-forward, stop, Info and Home (`KEY_ROOT_MENU`) each arrived as a key press and release on the `rc-cec` input, and the arrows also reached the focused terminal as keystrokes. The TV's own power key isn't forwarded, but SIMPLINK's device power-off is: it arrived as `KEY_POWER` (scan `0x40`) at 16:20:25, and the test PC entered suspend in the same second, so KDE's power-button action here is sleep. That's the hazard section 3 names, confirmed: passthrough without a `KEY_POWER` override lets anyone with the remote suspend the PC through SIMPLINK. the test PC resumed at 16:21:13, `inputattach` restarted after the resume (#212's fix, second time), and the daemon reclaimed without passthrough.
