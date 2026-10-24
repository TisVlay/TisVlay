# The TV controls the PC

TisVlay drives the TV from the PC. This plan adds the reverse direction. All of it is opt-in and off by default:
- the TV going to standby or waking up can lock or suspend the PC;
- on HDMI-CEC, the remote's "turn this device off" can act on the PC, the TV can wake a suspended PC, and the remote's keys can reach the desktop.

The decisions come from [docs/ideas/tv-controls-pc.md](../ideas/tv-controls-pc.md), after Checks A to C on an LG C3 with a Pulse-Eight adapter.

It has two parts:
- **A transport-agnostic core:** power events and host actions, built on a cause-stamped `state/power`. It works for CEC and LG over the network alike.
- **CEC extras:** the remote, and waking the PC from suspend. These exist only where the TV is on the CEC bus.

It builds on #212 (device availability, held wakes) and the Pulse-Eight fixes (#211, `88a3d854`).

## What each backend can do

| Feature | HDMI-CEC | LG over the network (SSAP) |
|---|---|---|
| TV went to standby | The TV's `<Standby>` / standby power-status broadcasts | The power-state subscription (`Active Standby`, `Suspend`) before the socket closes |
| TV woke | The on power-status broadcast | The power state back to `Active` |
| The remote's power key, aimed at the PC | SIMPLINK's device power-off, as `<User Control Pressed>` Power | None |
| The remote's other keys to the desktop | The kernel's `rc-cec` with passthrough (Check C) | None without an app on the TV (idea doc, LG remote keys) |
| The TV wakes a suspended PC | A firmware adapter (Pulse-Eight) with USB wakeup (Check B′) | None: the TV sends no Wake-on-LAN to its sources |

## Decisions

### Core (every backend)

- **PC-side actions are a new action type.** Profiles' `SessionAction` acts on the TV (screen off, power off, …). The reverse direction acts on the PC, so it gets its own `HostAction`: `none`, `lock` (the session), `suspend` (the system).
- **The events live in the user profile and run in the session service.** `on_tv_standby` and `on_tv_wake`, each a `HostAction`, default `none`. The session service runs in the user's session, can lock it and can suspend the system through logind without extra polkit rules, and already mirrors every TV's `state/power`. They honor the same gates as the session's other actions: the active user only, and `automate_when`.
- **They fire on a reported power transition, never on link loss.** A network TV's dropped connection (a Wi-Fi blip) says nothing about its power. Only a backend-reported transition counts: CEC's broadcasts, or LG's power-state subscription.
- **A cause stamp keeps TisVlay's own power changes out.** The TV reports the same transition whether its remote or TisVlay caused it (on CEC, identical broadcasts, Check A). So the world stamps each `state/power` transition with its cause, from the power commands it sent that TV. This is a wire change.

### CEC extras

- **The remote's power key is its own event.** SIMPLINK's device power-off reaches the daemon as `<User Control Pressed>` Power (`0x40`) addressed to the PC. The kernel queues it to followers whether or not passthrough is on. It becomes `on_remote_power` in the user profile: a `HostAction`, plus `tv_power_off`, where turning the TV off is a natural choice. It's explicit and never an echo, so it needs no cause stamp.
- **TisVlay gates the remote's keys; the desktop maps them.** A per-TV system-profile toggle, "send the remote's keys to the desktop", sets the passthrough flag on the claim. The kernel keymap and the desktop's shortcuts decide what keys mean. The keymap drops `0x40`, so the power key can't become `KEY_POWER` (which suspended pox in Check C). No per-key table in TisVlay.
- **"The TV may wake this PC" is a system-profile field, applied at each sleep.** Default off. The daemon can't write `/sys` (`ProtectKernelTunables=true`). At logind's `PrepareForSleep` it starts a root oneshot unit, through a narrow polkit rule allowing only that unit, and the unit sets the adapter's USB `power/wakeup` to the profile's value. No prompt, so it works at sleep time. Adapters without remote wakeup report it, and the field does nothing on them.

## Implementation

### Core

#### 1. Profile fields and `HostAction`

- [ ] `HostAction` (`none`, `lock`, `suspend`) in `tisvlay-core`, beside `SessionAction`; `#[non_exhaustive]` per the public API policy.
- [ ] User profile: `on_tv_standby` and `on_tv_wake` (`Option<HostAction>`, inheriting like the other fields; resolved default `none`).
- [ ] The GUI's profile pane and the CLI's profile verbs pick them up; the docgen profile-defaults partial regenerates.

#### 2. World: the cause stamp (wire)

- [ ] `state/power` gains `cause`: `requested` when the transition follows a power command TisVlay sent the TV, `external` otherwise. It covers profile dispatches, rpcs, wake dials and screen actions emulated as power, for every backend.
- [ ] One `feat` commit with the SDK surface.

#### 3. Session service: run the host actions

- [ ] A `state/power` transition to standby with `cause: external` runs `on_tv_standby`; to on with `cause: external` runs `on_tv_wake`. A connection drop without a reported transition runs nothing.
- [ ] `lock` locks the user's graphical session through logind; `suspend` calls logind's `Suspend`.
- [ ] Gated like the session's other actions (active user, `automate_when`).
- [ ] Loops can't form: an `on_tv_standby` suspend leads to a system sleep whose own profile action may power the TV off, which the stamp marks `requested`.

### CEC extras

#### 4. The remote power event (wire)

- [ ] The CEC connection's follower monitor turns a `<User Control Pressed>` Power addressed to the PC into a backend event; the world publishes it as a per-TV `event/remote-power`.
- [ ] User profile: `on_remote_power` (`HostAction` or `tv_power_off`, default `none`); the session service subscribes and runs it.

#### 5. The remote-keys gate

- [ ] A linux-cec setter for the log-address flags (`CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`). Upstream first (holo/linux-cec on gitlab.steamos.cloud), alongside the pending OSD-name issue ([docs/bugs/upstream-linux-cec-reclaim-drops-osd-name.md](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md)); until it lands, a vendored patch or a narrow ioctl (open question).
- [ ] System profile: `remote_keys_to_desktop` (`Option<bool>`, resolved default `false`). The CEC link sets the flag on the claim when it's on; a change re-claims.
- [ ] The keymap override dropping `0x40`: shipped by the NixOS module (a hwdb or `rc_keymaps` entry for the `rc-cec` map) and documented for other distros.

#### 6. The wake helper

- [ ] System profile: `tv_may_wake_pc` (`Option<bool>`, resolved default `false`).
- [ ] A root oneshot `tisvlay-wake@.service` that resolves a CEC node to its USB adapter (`/sys/class/cec/cecN/device` → serio → tty → USB device) and writes the requested `power/wakeup`, read from a file the daemon writes under its runtime directory.
- [ ] A polkit rule letting the daemon's user start only that unit, with no prompt.
- [ ] At `PrepareForSleep(true)`, the daemon writes each CEC TV's resolved value and starts the unit before the sleep proceeds, inside the inhibitor window it already holds for the sleep dispatch.
- [ ] The NixOS module installs the unit and the rule; the `.deb`/`.rpm`/Arch packages ship them too.

### 7. GUI and docs

- [ ] Docs: architecture.md (the reverse direction, the cause stamp), protocol.md (remote keys, SIMPLINK power-off), the packaging notes (the helper, the polkit rule, the keymap). The user guide waits until the CEC follow-ups near their end.
- [ ] The TV pane shows, read-only, whether the TV can wake this PC (open question).

## Verification

- [ ] World and session tests:
  - the cause stamp across dispatches, rpcs, wake dials and emulated screen actions, on a network-shaped and a local-device mock;
  - a dropped connection running nothing;
  - each host action fired by its event, gated and not chaining;
  - the remote power event.
- [ ] Hardware, on the LG C3, run as unattended blocks since the TV is pox's display:
  - `on_tv_standby` / `on_tv_wake` over CEC (Check A's sequence) and over SSAP, with the stamp;
  - Check B′ with `tv_may_wake_pc` on and off, applied by the helper;
  - Check C with the gate on and the power key dropped.

## Open questions

- **The linux-cec flag setter:** upstream only, or a vendored patch until it lands?
- **The cause stamp's window:** a fixed window after a TisVlay power command, or a stamp held until the TV's next reported state?
- **Lock versus suspend:** does `suspend` lock first, or is that the desktop's own setting?
- **Surfacing:** should the GUI show whether the adapter can wake this PC?
- **The label across long armed suspends:** this decides whether the helper should also disarm the Pulse-Eight on suspend (the module parameter).
