# The TV controls the PC

TisVlay drives the TV from the PC. This plan adds the reverse direction. All of it is opt-in and off by default:
- the TV going to standby or waking up can lock or suspend the PC;
- on HDMI-CEC, the remote's "turn this device off" can act on the PC, the TV can wake a suspended PC, and the remote's keys can reach the desktop.

The decisions come from [docs/ideas/tv-controls-pc.md](../ideas/tv-controls-pc.md), after Checks A to C on an LG C3 with a Pulse-Eight adapter.

It has two parts:
- **A transport-agnostic core:** power events and host actions, built on a cause-stamped `state/power`. It works for CEC and LG over the network alike.
- **CEC extras:** the remote, and waking the PC from suspend. These exist only where the TV is on the CEC bus, and the backend declares them as capabilities.

It builds on #212 (device availability, held wakes) and the Pulse-Eight fixes (#211, `88a3d854`).

## What each backend can do

| Feature | HDMI-CEC | LG over the network (SSAP) |
|---|---|---|
| TV went to standby | The TV's `<Standby>` / standby power-status broadcasts | The power-state subscription (`Active Standby`, `Suspend`) before the socket closes |
| TV woke | The on power-status broadcast | The power state back to `Active` |
| The remote's power key, aimed at the PC | SIMPLINK's device power-off, as `<User Control Pressed>` Power | None |
| The remote's other keys to the desktop | The kernel's `rc-cec` with passthrough (Check C) | None without an app on the TV (idea doc, LG remote keys) |
| The TV wakes a suspended PC | A firmware adapter (Pulse-Eight) with USB wakeup (Check B′) | None: the TV sends no Wake-on-LAN to its sources |

## Where the settings live

Two questions place every setting (decided 2026-10-25):
1. **Is it a fact or a choice?** A fact about the setup (which adapter, which input is the PC) goes in the TV's device config. A choice about behavior goes in a profile, never as a device-config bool.
2. **For a choice, whose scope does the action touch?** An action on the machine (suspending it, letting the TV wake it, what the shared adapter forwards) goes in the system profile and runs in the daemon, so it works with nobody logged in and a shared machine doesn't depend on one user's settings. An action on the user's session (locking it) goes in the user profile and runs in the session service, for the active user.

What a backend can honor is neither: it's a capability the backend declares, the way `INPUT` already decides whether `restore_input` does anything. The profile fields stay backend-neutral, and a field a TV's backend can't honor does nothing for that TV.

| Setting | Home | Field and type | Needs |
|---|---|---|---|
| TV standby or wake → lock | User profile | `on_tv_standby`, `on_tv_wake`: `UserHostAction` | `state/power` (every backend) |
| TV standby or wake → suspend | System profile | `on_tv_standby`, `on_tv_wake`: `SystemHostAction` | `state/power` (every backend) |
| The remote's power key → lock | User profile | `on_remote_power`: `UserHostAction` | `REMOTE_POWER` |
| The remote's power key → suspend | System profile | `on_remote_power`: `SystemHostAction` | `REMOTE_POWER` |
| The TV may wake this PC | System profile | `tv_may_wake_pc`: `bool` | `WAKE_HOST` |
| The remote's keys reach the desktop | System profile | `remote_keys_to_desktop`: `bool` | `REMOTE_KEYS` |
| The power key never becomes `KEY_POWER` | Host config | the `rc-cec` keymap | none (static, not TisVlay's) |

Two existing CEC device-config fields predate the rule and are choices: `emulate_screen_via_power` and `advertise_osd_name`. They stay as they are in this plan; the idea doc tracks moving them. `osd_name` is a fact (what this PC is called on the bus) and stays.

## Decisions

### Core (every backend)

- **Host actions are typed per scope.** Profiles' existing action, `SessionAction`, acts on the TV (screen off, power off, …). The reverse direction acts on the PC, with one type per scope: `UserHostAction` (`none`, `lock`) in the user profile and `SystemHostAction` (`none`, `suspend`) in the system profile. A user profile can't express a suspend at all, so there's nothing to validate, and each dispatcher's match covers only what its scope can do. They're named after the profiles because `SessionAction` already names the TV-side type.
- **Each scope runs its own.** The session service runs the user profile's host actions: it runs in the user's session, can lock it, and already mirrors every TV's `state/power`. It applies the same gates as the session's other actions: the active user only, and `automate_when`. The daemon's world runs the system profile's, alongside its other machine events and with the same gates (`managed`, `automate_when`). It suspends through logind, which needs a polkit rule for the daemon's user, since the daemon runs outside any session. That way a suspend works at the login screen and with nobody logged in.
- **Lock and suspend compose.** Setting both runs both, each from its own service. Their order isn't guaranteed, so a lock that must land before the sleep comes from the desktop's own lock-on-sleep setting.
- **They fire on a reported power transition, never on link loss.** A network TV's dropped connection (a Wi-Fi blip) says nothing about its power. Only a backend-reported transition counts: CEC's broadcasts, or LG's power-state subscription.
- **A cause stamp keeps TisVlay's own power changes out.** The TV reports the same transition whether its remote or TisVlay caused it (on CEC, identical broadcasts, Check A). So the world stamps each `state/power` transition with its cause, from the power commands it sent that TV. A command records the transition it expects, and the first matching report consumes it as `requested`. The expectation expires after a bound, so a command the TV ignored can't mislabel a later press of its remote. This is a wire change.

### CEC extras

- **Backend support is a capability.** Three new capability flags, declared per connection and config-aware at the daemon, the way the daemon already adds `SCREEN` for a screen-via-power CEC TV:
  - `REMOTE_POWER`: the remote's power key reaches the PC. Every CEC connection has it.
  - `REMOTE_KEYS`: the remote's keys can reach the desktop. An adapter that registers an `rc-cec` device (the kernel's `CEC_CAP_RC`).
  - `WAKE_HOST`: the TV can wake this PC. An adapter whose USB device can wake the system, read from its `power/wakeup` at claim. DisplayPort-AUX adapters don't qualify.

  The world skips a field for a TV without its capability, and the GUI shows them on the TV pane, so a user can see why a setting does nothing there. Adding flags is a wire change.
- **The remote's power key is its own event.** SIMPLINK's device power-off reaches the daemon as `<User Control Pressed>` Power (`0x40`) addressed to the PC. The kernel queues it to followers whether or not passthrough is on. It becomes `on_remote_power` in both profiles, each with its scope's host action. It's explicit and never an echo, so it needs no cause stamp. The earlier `tv_power_off` choice is dropped: the host actions already reach the TV through the PC's own events. A suspend runs the system profile's `on_sleep`, which powers the TV off by default, and a lock runs the user profile's `on_session_lock`.
- **TisVlay gates the remote's keys; the desktop maps them.** A system-profile toggle, `remote_keys_to_desktop`, sets the passthrough flag on the claim. The kernel keymap and the desktop's shortcuts decide what keys mean. The keymap drops `0x40`, so the power key can't become `KEY_POWER` (which suspended pox in Check C). No per-key table in TisVlay.
- **"The TV may wake this PC" is a system-profile field, applied at each sleep.** Default off. The daemon can't write `/sys` (`ProtectKernelTunables=true`). At logind's `PrepareForSleep` it starts a root oneshot unit, through a narrow polkit rule allowing only that unit, and the unit sets the adapter's USB `power/wakeup` to the profile's value. No prompt, so it works at sleep time.

## Implementation

### Core

#### 1. Profile fields and host actions

- [ ] `UserHostAction` (`none`, `lock`) and `SystemHostAction` (`none`, `suspend`) in `tisvlay-core`, beside `SessionAction`; `#[non_exhaustive]` per the public API policy.
- [ ] User profile: `on_tv_standby` and `on_tv_wake` (`Option<UserHostAction>`). System profile: `on_tv_standby` and `on_tv_wake` (`Option<SystemHostAction>`). Both inherit like the other fields; resolved default `none`.
- [ ] The GUI's profile panes and the CLI's profile verbs pick them up; the docgen profile-defaults partial regenerates.

#### 2. World: the cause stamp (wire)

- [ ] `state/power` gains `cause`: `requested` when the transition follows a power command TisVlay sent the TV, `external` otherwise. It covers profile dispatches, rpcs, wake dials and screen actions emulated as power, for every backend.
- [ ] A power command records the transition it expects when it's sent to the TV (a held wake records it when it's finally sent), one expectation per direction. The first reported transition in that direction is stamped `requested` and consumes it; any other is `external`. An expectation expires after 60 s, the held wake's bound, and a newer command in the same direction renews it. Per direction, so a power-off quickly followed by a power-on still stamps the late standby report `requested`.
- [ ] One `feat` commit with the SDK surface.

#### 3. Run the host actions

- [ ] A `state/power` transition to standby with `cause: external` runs `on_tv_standby`; to on with `cause: external` runs `on_tv_wake`. A connection drop without a reported transition runs nothing.
- [ ] Session service: the user profile's `lock` locks the user's graphical session through logind, gated like the session's other actions (active user, `automate_when`).
- [ ] World: the system profile's `suspend` calls logind's `Suspend` from the daemon, gated like the system profile's other events (`managed`, `automate_when`).
- [ ] A polkit rule lets the daemon's user suspend without a prompt; the NixOS module and the `.deb`/`.rpm`/Arch packages always ship it. The profile field, default `none`, is the opt-in: a second switch at install time would be one a GUI user can't see, so the setting would silently do nothing. The cost is that whoever may edit system profiles (the `tisvlay` group) can have the machine suspend.
- [ ] Loops can't form: an `on_tv_standby` suspend leads to a system sleep whose own profile action may power the TV off, which the stamp marks `requested`.

### CEC extras

#### 4. Host capabilities (wire)

- [ ] `REMOTE_POWER`, `REMOTE_KEYS` and `WAKE_HOST` in `Capabilities`, set for CEC connections as above (`WAKE_HOST` resolves the CEC node to its USB adapter, `/sys/class/cec/cecN/device` → serio → tty → USB device, and reads it).
- [ ] The world skips `on_remote_power`, `remote_keys_to_desktop` and `tv_may_wake_pc` for a TV without the capability each needs.
- [ ] One `feat` commit.

#### 5. The remote power event (wire)

- [ ] The CEC connection's follower monitor turns a `<User Control Pressed>` Power addressed to the PC into a backend event; the world publishes it as a per-TV `event/remote-power`.
- [ ] `on_remote_power` in both profiles, each with its scope's type, default `none`. The world runs the system profile's; the session service subscribes and runs the user profile's.

#### 6. The remote-keys gate

- [ ] A linux-cec setter for the log-address flags (`CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`). Upstream first (holo/linux-cec on gitlab.steamos.cloud), alongside the pending OSD-name issue ([docs/bugs/upstream-linux-cec-reclaim-drops-osd-name.md](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md)); until it lands, a vendored patch or a narrow ioctl (open question).
- [ ] System profile: `remote_keys_to_desktop` (`Option<bool>`, resolved default `false`). The CEC link sets the flag on the claim when it's on and the TV has `REMOTE_KEYS`; a change re-claims.
- [ ] The keymap override dropping `0x40`: shipped by the NixOS module (a hwdb or `rc_keymaps` entry for the `rc-cec` map) and documented for other distros.

#### 7. The wake helper

- [ ] System profile: `tv_may_wake_pc` (`Option<bool>`, resolved default `false`).
- [ ] A root oneshot `tisvlay-wake@.service` that resolves a CEC node to its USB adapter (as in step 4) and writes the requested `power/wakeup`, read from a file the daemon writes under its runtime directory.
- [ ] A polkit rule letting the daemon's user start only that unit, with no prompt, in the same rules file as step 3's.
- [ ] At `PrepareForSleep(true)`, the daemon writes the resolved value for each TV with `WAKE_HOST` and starts the unit before the sleep proceeds, inside the inhibitor window it already holds for the sleep dispatch.
- [ ] The NixOS module installs the unit and the rule; the `.deb`/`.rpm`/Arch packages ship them too.

### 8. GUI and docs

- [ ] Docs: architecture.md (the placement rule from [Where the settings live](#where-the-settings-live), the reverse direction, the cause stamp), protocol.md (remote keys, SIMPLINK power-off), the packaging notes (the polkit rules, the helper, the keymap). The user guide waits until the CEC follow-ups near their end.
- [ ] The TV pane shows the TV's host capabilities read-only, and the profile panes' help for `on_remote_power`, `remote_keys_to_desktop` and `tv_may_wake_pc` names the capability each needs.

## Verification

- [ ] World and session tests:
  - the cause stamp across dispatches, rpcs, wake dials and emulated screen actions, on a network-shaped and a local-device mock;
  - a dropped connection running nothing;
  - each scope's host action fired by its event, gated and not chaining;
  - a field doing nothing for a TV without its capability;
  - the remote power event in each scope.
- [ ] Hardware, on the LG C3, run as unattended blocks since the TV is pox's display:
  - `on_tv_standby` / `on_tv_wake` over CEC (Check A's sequence) and over SSAP, with the stamp;
  - a system-profile `on_tv_standby` suspend at the login screen, with nobody logged in;
  - Check B′ with `tv_may_wake_pc` on and off, applied by the helper;
  - Check C with the gate on and the power key dropped.

## Open questions

- **The linux-cec flag setter:** upstream only, or a vendored patch until it lands?
- **The label across long armed suspends:** this decides whether the helper should also disarm the Pulse-Eight on suspend (the module parameter).
