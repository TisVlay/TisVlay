# TV-driven PC power and input

TisVlay drives the TV from the PC: PC events (boot, sleep, lock, idle, DPMS) become TV actions. The reverse direction has openings on every backend and more on HDMI-CEC:
- **Any backend:** the TV powering off can lock the session or put the PC to sleep. CEC reports it in broadcasts, LG over the network through its power-state subscription.
- **CEC only:** the TV's power key can wake a sleeping PC, through a Pulse-Eight adapter's firmware; and the TV's remote can drive the desktop, through the kernel's CEC core.

They're grouped here because they share a trigger (a user asks for "the TV controls the PC") and a design question: what TisVlay owns, what's host configuration, and what's left to the TV and the desktop. The decided parts are planned in [docs/plans/tv-controls-pc.md](../plans/tv-controls-pc.md), as prerequisites, a transport-agnostic core and CEC extras. This doc keeps the investigation, the hardware checks and what isn't planned; most of it was written from the CEC investigation, and [Not yet planned](#not-yet-planned) tracks what generalizes further. Names follow [power-vocabulary.md](power-vocabulary.md); the Check records keep the kernel's and logind's own words.

Backend model: [docs/cec/protocol.md](../cec/protocol.md). Adapter lifecycle: [docs/watchers/devices.md](../watchers/devices.md). The Pulse-Eight's autonomous mode and its label trade-off: [docs/bugs/cec-pulse8-autonomous-label.md](../bugs/cec-pulse8-autonomous-label.md).

## What exists today

| Direction | Mechanism already there | Gap |
|---|---|---|
| TV wakes the PC | The Pulse-Eight firmware's autonomous mode, armed on every claim and kept armed through a sleep (#211 disarms only on disconnect) | Whether its wake reaches the PC (the adapter's USB wakeup is disabled here); no way to choose it; nothing for non-Pulse-Eight adapters |
| TV sleeps or locks the PC | The CEC connection's monitor turns the TV's own broadcasts (`<Standby>`, `<Report Power Status>`) into `state/power`, and the session service already mirrors `state/power` | No event or action consumes a TV-side power change; TisVlay can't tell the TV's own standby from one it caused |
| TV remote as input | The kernel's `rc-cec` device and keymap, registered when the adapter attaches | The kernel forwards keys only when the claim allows RC passthrough, which TisVlay's claim doesn't (and linux-cec can't yet set) |

## Decisions

Taken 2026-10-24 after Checks A to C ran, revised 2026-10-25 for where settings live, the vocabulary and a four-area review; they live in [the plan's Decisions](../plans/tv-controls-pc.md#decisions). The sections below keep the investigation and the options that were weighed.

## 1. The TV wakes the PC

Only the adapter is awake while the PC sleeps: TisVlay's processes are frozen, so the waking itself is firmware plus host power plumbing, and TisVlay's part is arming it before each sleep (the plan's wakeup unit) and docs.

**The Pulse-Eight's side.** The driver's header notes say that in autonomous mode the dongle "is known to attempt wakeup" on `<User Control Pressed>` Power, Power On or Power Toggle, on `<Set Stream Path>` to its own physical address, and on LG's vendor command `0x03 0x00`. Upstream's design (`e28a6c8b3fcc`, 2016-08-22, Johan Fjeldtvedt) is exactly this: the claim's settings are written to the adapter so it keeps operating "when it is disconnected from the driver (typically by going into suspend)". The PC's sleep doesn't disconnect the driver, so #211's disarm doesn't touch it; but the firmware only takes over once it misses the driver's pings (every 15 s; the label flipped about 60 s after a detach), so a wake attempt needs the PC to have been asleep that long.

**The host's side.** The dongle's wake has to travel as a USB remote wakeup: the device must advertise it, the kernel must have it enabled for that device, and the controller must be allowed to wake the system from the sleep state in use. Read on pox (2026-10-24):

| Item | Value | Meaning |
|---|---|---|
| Adapter `bmAttributes` (`/sys/bus/usb/devices/1-5`) | `a0` | Bit `0x20`: the adapter supports remote wakeup |
| Adapter `power/wakeup` | `disabled` | Capable, but not allowed to wake the system (the file exists only for wake-capable devices) |
| Root hub `usb1` `power/wakeup` | `disabled` | May need enabling too; part of the check below |
| xHCI controller `0000:00:14.0` `power/wakeup` | `enabled` | The controller may wake the system |
| `/proc/acpi/wakeup` | `XHCI S4 *enabled` | ACPI allows the controller to wake from states up to S4 |
| `/sys/power/mem_sleep` | `s2idle [deep]` | Suspend is S3 |

This read assumed the adapter honors its `power/wakeup`; it doesn't (the correction below). With it enabled for Check B, pox did resume when the TV's power key was pressed, but three other devices on the same bus were also allowed to wake it, so the resume isn't attributed to the dongle yet (Check B result, and Check B′ to settle it).

**The wake trigger is already on the bus.** Check A showed the TV turning on with its remote broadcasts `<Set Stream Path>` to the input it lands on (`0f 86 10 00` for HDMI 1, the PC's adapter at 1.0.0.0). That's one of the messages the firmware notes say it wakes on, and it's a policy signal too: it names the input, so "wake the PC only when the TV comes up on the PC's input" falls out of it, where "wake on any TV power-on" would also wake the PC for the Chromecast on HDMI 4.

**The trade-off.** Keeping the dongle armed while suspended is what lets it wake the PC, and it's also what lets it answer the TV on its own with LG's vendor ID, which the bug doc showed pins "LG DVD Player" on the input until the TV re-detects the device. A sleep long enough for the firmware to take over may cost the label each time (Check B reads it after the PC wakes). Check B′ seemed to show the wake doesn't need the dongle armed (it woke pox undriven and disarmed), but the disarm clears only the adapter's working state, and its EEPROM decides the wake (the correction below), so the two do conflict.

**The knobs.** Two switches, decided apart:
- **Whether the dongle stays armed while the PC sleeps** is host configuration, never a TisVlay profile setting: a driver setting, Pulse-Eight-specific, root-only. If it's ever needed, it's a `pulse8-cec` module parameter that skips arming on claim, plus a NixOS option beside `cec.pulse8.enable` ([docs/plans/cec-pulse8-autonomous.md](../plans/cec-pulse8-autonomous.md)).
- **Whether the TV may wake the PC** is a TisVlay choice, runtime from the start (2026-10-24): the system profile's `on_tv_power_on`, which the plan's wakeup unit applies to the adapter's `power/wakeup` before each sleep. That switch turned out not to control this adapter (the correction below).

**Correction (2026-10-31): the adapter's EEPROM decides.** The Pulse-Eight raises the USB remote wakeup whatever its `power/wakeup` says, and whether it wakes the PC follows the autonomous flag in its EEPROM, not the one the driver sets on each claim. Evidence, timed runs and sources (libcec #546 and libcec 8's `bAutonomousMode`, the kernel's `USB_QUIRK_DISCONNECT_SUSPEND`): [the plan's step 8b](../plans/tv-controls-pc.md#8b-the-adapters-eeprom-decides-the-wake-found-on-hardware). So the two knobs above collapse into one firmware setting, which the driver doesn't expose; TisVlay's lever is the wakeup unit's port cut; the EEPROM lever and a kernel patch are below.

**The EEPROM lever (proven 2026-10-31, not adopted).** The `pulse8_cec` driver's `persistent_config=1` parameter makes the adapter's EEPROM follow the claim: its 15 s ping work writes the adapter's state after every claim change, so a claim stores autonomous mode on and a clear stores it off (read back after USB power cycles: on, then off). With the claim cleared and the write landed, the PC stayed asleep through SIMPLINK's power press; claimed, it woke. So the daemon could choose the wake per sleep by claiming or clearing, keeping the adapter on USB in the vendor's supported "don't wake" state. The plan's step 8b ships the port cut instead, because:
- the write lands up to 15 s after the clear, on the next ping, and userspace can't see it land; logind's delay inhibitor allows 5 s by default, so the clear would have to come from the root unit with a fixed wait;
- every host needs the module option, and each claim change costs an EEPROM write (two per sleep, plus each reconnect);
- at attach the driver restores the stored claim itself, and a daemon dial that raced it failed once on the kernel's in-progress address (`Invalid value 255 for type LogicalAddress`) before its retry connected;
- it's Pulse-Eight-only, where the port cut covers any USB adapter that wakes the PC unasked.

The EEPROM also keeps the daemon's name and address, so an unattended adapter would answer as "TisVlay"; whether that keeps the input's label is untested.

**A kernel patch (not pursued).** The cleaner route is in the driver, which our out-of-tree build already patches (the disarm on disconnect): write the autonomous flag to the EEPROM as soon as it changes rather than on the next ping, or let the claim say whether the adapter stays autonomous, so the daemon's choice takes effect before the sleep without a root unit. Either is a linux-media patch with the same review path as the disarm ([cec-pulse8-autonomous.md](../plans/cec-pulse8-autonomous.md)). Revisit if the port cut's costs (the adapter off USB and re-enumerated each sleep, a possibly pinned label) show up in use.

**Other adapters (unverified).** DisplayPort-AUX CEC on amdgpu or i915 is driven by the GPU driver over the DP AUX channel, so it should go quiet with the GPU when the PC sleeps, with no firmware of its own to answer the TV or raise a wake. Nothing here verified it; the path for those hosts would be another wake source (Wake-on-LAN from a phone, a USB keyboard), not CEC.

## 2. The TV sleeps or locks the PC

**The signal.** When the TV goes to standby it broadcasts `<Standby>` (`0f 36`) and then its power status as standby (`0f 90 01`); when it comes back it broadcasts `0f 90 00` and a `<Set Stream Path>` to the input it lands on (Check A). The CEC monitor already turns TV-originated broadcasts into `state/power` (it ignores other initiators). An LG over the network reports the same transitions through SSAP, so the event is transport-agnostic: "the TV changed power state, and TisVlay didn't ask it to". The wake side can carry more on CEC: whether the TV came up on this PC's input, from the Set Stream Path address.

**The event and the action.** Every power event maps to a configurable profile action, never a hardcoded one, so this is a pair of new events with a new action vocabulary: the existing `EventAction`s act on the TV, these act on the PC. `on_tv_power_off` exists in both profiles, each taking its scope's PC action: lock the session in the user profile, sleep in the system profile (`suspend`, or `sleep` for the host's configured mode), or nothing. `on_tv_power_on` exists only in the system profile, as the TV waking a sleeping PC (section 1): lock or sleep make no sense when the TV comes on. Defaults: nothing, so it's strictly opt-in.

**Who acts.** Each scope's own service. The session service locks: it runs in the user's session and already mirrors every TV's `state/power` (for the idle drift re-assert). The daemon puts the PC to sleep: it's a system user outside any session, so it needs a polkit rule for logind's sleep methods, but it runs at the login screen and with nobody logged in, where no session service does. The first sketch put both in the user profile, since logind lets an active session put the system to sleep without extra rules; that left sleep to one user's settings on a shared machine, and to nothing when nobody's logged in.

**The origin problem.** TisVlay itself turns TVs off: the system profile's sleep and shutdown actions, idle and lock actions (screen-off is emulated as power-off on CEC), a user's `power off`. The TV then broadcasts exactly the same pair as for its own remote, `0f 36` then `0f 90 01` (Check A against a traced power-off where it followed our `40 36` in the same second): nothing in the TV's broadcast says who asked. So a naive event would chain "idle → TV off → PC sleep", or fire during the PC's own sleep. The event needs to know the transition wasn't TisVlay's. Two options: the world stamps each power transition with its cause (it knows the power commands it sent each TV), or the session suppresses the event for a window after any power command it can see (simpler, but blind to the daemon's own system-profile dispatches, so it doesn't work alone). The stamp is the sound one. The review (2026-10-25) added the second half: a connection's first reading isn't a transition either, so the world, which can tell the two apart, decides the edge and publishes it, and the stamp stays inside the world.

**Hazards.** A TV that sleeps on its own timer (no-signal auto-off, eco settings) would also fire it, which is probably what a user who opts in wants, but it should be said. The unconfirmed C3 behavior of ignoring `<Standby>` just after waking ([docs/concerns/cec-standby-ignored-after-wake.md](../concerns/cec-standby-ignored-after-wake.md)) doesn't affect this direction.

## 3. The TV remote as input

**What's there (read on pox, 2026-10-24).** The kernel registered `rc0` (`rc-cec` keymap, driver `cec`, protocol `cec` enabled, input node `/dev/input/event26`, owned `root:input`), with no `wakeup_protocols`, so it can't be a wake source itself. The keymap covers navigation (`KEY_UP`…`KEY_OK`), digits, menus, media keys, volume and mute, and maps CEC's Power key (`0x40`) to **`KEY_POWER`**. On this desktop PowerDevil holds the power-key inhibitor, and its AC `PowerButtonAction` is `1`, which reads as sleep (assuming PowerDevil's action numbering; not verified). The Pulse-Eight's separate USB HID interface (`input11`, a mouse) is unrelated to CEC keys.

**Why no keys arrive today.** The kernel forwards `<User Control Pressed>` to `rc-cec` only when the claim sets `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`. TisVlay's claim doesn't: `cec-ctl -d /dev/cec0` shows `Logical Addresses : 1` with no flag, while a `cec-ctl --playback` claim shows `(Allow RC Passthrough)`. linux-cec 0.2.1 has no setter for log-address flags, so enabling it would need a linux-cec change, like the missing `all_device_types` and `cec_version` setters. The TV also only sends remote keys to the active source, and on an LG only when SIMPLINK is on and the PC's input is selected. The follower queue gets `<User Control Pressed>` either way: in `cec_receive_notify` the passthrough case only adds the `rc_keydown` before falling through to the follower queue (kernel 7.3 source), which is what the plan's remote power key relies on.

**Options.**
- **Stay out (today).** Document that remote keys don't reach the desktop under TisVlay, and why.
- **Opt-in passthrough.** A per-TV setting that sets the flag on the claim (once linux-cec can), so the remote drives the desktop through the kernel's keymap with no TisVlay key handling. The `KEY_POWER` mapping is the hazard: a remote power press would put this PC to sleep. Defang it with a keymap override (an `rc_keymaps` file or a hwdb entry dropping `0x40`), shipped in every package, rather than filtering keys in TisVlay. This is the one chosen.
- **TisVlay maps keys itself.** Read `<User Control Pressed>` from the follower monitor and act (volume to the PC, media keys over MPRIS). More control, much more surface: a per-key table would duplicate input-remapper, keyd, hwdb and the desktop's shortcuts, and need input injection for navigation. Only if the kernel path proves wrong; if a need appears, a small set of TisVlay *intents* fits better than raw keys.

## 4. Install and configuration

- **TisVlay itself.** Profile fields, editable in the GUI's profile panes like the others: lock in the user profile; sleep, the TV waking the PC and the remote's keys in the system profile. The TV pane shows the capabilities read-only (`WAKES_PC` from the adapter's `power/wakeup`), so a user can see why "the TV wakes the PC" doesn't.
- **Packaging.** The plan's polkit rule, wakeup unit and keymap override ship in the NixOS module and every package. Host configuration is left only for the Pulse-Eight's autonomy (the module parameter above, if ever needed) and, on other distros, the CEC setup itself ([docs/ideas/cec-distro-setup.md](cec-distro-setup.md)).
- **User guide** (held until the CEC follow-ups near their end): what "the TV controls the PC" can and can't do per adapter type, and the label trade-off in plain words.

## Checks to run

The checks (A: the TV's own power key; B and B′: the TV waking the PC; C: the remote's keys) and their results moved to [docs/cec/checks.md](../cec/checks.md).

## Not yet planned

Raised while generalizing beyond CEC (2026-10-24). Not mature enough for the plan; picked up when the planned core lands or a user asks.

- **Input-change events.** Both backends report which input the TV shows: CEC through `<Set Stream Path>` and `<Active Source>` (Check A saw `0f 86 10 00` at power-on), LG through its foreground-app subscription, which the world already reads for `primary_input` and `automate_when: on_primary`. That gives `on_tv_left_pc` (switched to the Chromecast: lock, pause media, blank) and `on_tv_returned_to_pc` (wake the screen), with no new data source.
  - **Wake the screen** is the user-side reaction to the TV coming back: wake the desktop out of DPMS-off, so the TV shows the lock screen instead of "No signal" and doesn't drop back to standby. It never unlocks. CEC expects it too: a playback device that sees Set Stream Path to itself is meant to wake. The mechanism differs per desktop and needs research: KDE has `org.freedesktop.ScreenSaver.SimulateUserActivity`, GNOME has its own, and Wayland has no standard way to fake activity. First check whether KWin already relights its output when the TV's HDMI comes back.
  - **A user-profile `on_tv_power_on` with the same action was considered and deferred (2026-10-25).** It would fire on any power-on, including onto another input, and would miss a switch back to the PC with the TV already on. `on_tv_returned_to_pc` is the precise trigger.
- **Screen-off events.** LG reports its own screen-off mode (`Screen Off`, the OLED screen-off from its menu or remote) apart from standby; "screen off → lock the PC" is a natural pairing. CEC has no screen-off state of its own (the backend emulates screen as power).
- **More than one TV.** With two TVs on one PC (the C3 and the G4 here: sometimes both show pox, more often one at a time), one TV's standby shouldn't put the PC to sleep or lock it while the other is in use. The plan has no guard (decided 2026-10-25): each TV acts alone, and per-TV profile assignment decides which TVs' standby acts at all. A guard would ask whether another TV still shows the PC, with these questions open:
  - **Which TVs block.** Any TV that's on? One that's on and on its `primary_input`, the test `automate_when: on_primary` uses? LG knows its input, but CEC reports none, so a CEC TV that's on would have to count as showing the PC: it fails safe, the reverse of `automate_when`'s benefit of the doubt. Or only TVs whose profile sets the same PC action ("when the last of them goes off"), or only the TV the EDID probe matches to the watched connector?
  - **Whether it covers lock too.** The G4 going to standby shouldn't lock the session in use on the C3 either, though a lock is cheaper to undo than a sleep.
  - **Where it runs.** The daemon's world sees every TV, and the session service mirrors every TV's `state/power`, so either scope can apply it.
- **No unlock.** PC actions should never include unlocking: turning the TV on proves nothing about who's there. Anyone who can turn it on could open the PC: its remote, its phone app, Wake-on-LAN from any device on the network, or any HDMI device over CEC, which has no authentication and lets a device fake the TV's power report. (The other half of this item, never acting while a stream arms the TV, is now planned as a fixed gate.)
- **LG power-offs while unconnected.** A network TV that goes down stays down until asked, so one turned back on with its own remote isn't reconnected, and its next power-off goes unseen. Redialing on the TV's SSDP alive `NOTIFY` for TVs with a PC action set would close it ([ssdp-notify-passive-listener.md](ssdp-notify-passive-listener.md)).
- **Behavior knobs in CEC's device config.** Two fields predate the placement rule and are choices, not facts: `emulate_screen_via_power` (dispatch screen actions as power) and `advertise_osd_name` (whether to set the OSD name at all). The rule's shape for them is a capability plus a profile choice, or a split where the fact is the backend's and the choice the profile's. Moving them is a breaking config change, so it waits for the capability work here or a user hitting the gap. [cec-followups.md](cec-followups.md)'s emulated-screen toggle is the related client-side polish. `osd_name` is a fact (what this PC is called on the bus) and stays.
- **LG remote keys and pointer, without CEC.** Researched 2026-10-24: stock SSAP has no way to *receive* the remote's keys or pointer. The libraries ([lgtv2](https://github.com/hobbyquaker/lgtv2), [Home Assistant's webostv](https://www.home-assistant.io/actions/webostv.button/)) only send them, and the "pointer input socket" drives the TV's own pointer. The webOS homebrew docs list no key or remote service, and non-system apps reach only public Luna endpoints ([webOS Homebrew: Luna service bus](https://www.webosbrew.org/pages/luna-service-bus)). Receiving the Magic Remote takes an app running on the TV. [MagicRemoteService](https://github.com/Cathwyler/MagicRemoteService) (GPL-3.0, Windows PC side) installs a webOS app in developer mode, captures the remote through webOS DOM events, and forwards them over an unencrypted WebSocket. It maps clicks to gestures: a short OK press for a left click, a long press or the yellow key for a right click, and the scroll wheel forwarded. For TisVlay that would be a new component (a webOS app) with real costs: developer mode and its renewals, a re-install after most TV updates, and transport security to add. Low priority; CEC already carries the keys (not the pointer) where an adapter exists.
- **A value that leaves the adapter alone.** `on_tv_power_on` has two values: `wake` arms the adapter, and `none`, the default, cuts its USB port for the sleep (plan step 8b). A third value would leave the adapter as the host has it, so a Pulse-Eight with its EEPROM's autonomous mode on wakes the PC as it does out of the box, and a user's own udev rule or EEPROM setting stands. Its name is the hard part: in every other field `none` means TisVlay does nothing, here it acts, and the candidates for the blocking value (`keep_asleep`, `stay_asleep`, `ignore`, `block`) each either hide that TisVlay acts or name the mechanism. Decided 2026-10-31 to keep two values. Unmanaging the TV gives that state today (an unmanaged TV's adapter is left alone), at the cost of its other profile actions; reopen when a user wants the adapter's own behavior back on a managed TV.

## Open questions

- **The wake against the label.** Should the dongle stay armed while the PC sleeps, accepting a possibly pinned label after each long sleep, or should a sleeping PC disarm it too? The wake does need the dongle armed, in its EEPROM (section 1's correction), so disarming costs the wake; the plan's step 8b weighs the levers. So far: "TisVlay" after a 105 s and a 48 s sleep, and no pinned label noticed across longer everyday sleeps (2026-10-25). No check is planned; it's watched in daily use.
- **The TV waking a hibernated PC.** Hibernation is a full power-off to the firmware, so whether the dongle even has power in S4 depends on the board. A control run (does pox hibernate and come back at all) and a TV-key run are pending.

## Trigger

A user asks for the TV to wake, sleep or lock the PC, or to drive the desktop with its remote; or Checks A to C run and settle which of the sections are viable.
