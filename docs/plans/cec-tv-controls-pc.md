# The TV controls the PC

TisVlay drives the TV from the PC. This plan adds the reverse direction over HDMI-CEC, as decided in [docs/ideas/cec-tv-controls-pc.md](../ideas/cec-tv-controls-pc.md) § Decisions after Checks A to C on an LG C3 with a Pulse-Eight adapter:

- the TV going to standby or waking can lock or suspend the PC;
- SIMPLINK's "turn this device off" can act on the PC;
- the TV can wake a suspended PC, configurable at runtime;
- the TV remote's keys can reach the desktop.

Every one of these is opt-in and off by default.

It builds on #212 (device availability, held wakes) and the Pulse-Eight fixes (#211, `88a3d854`).

## Decisions (from the idea doc)

- **PC-side actions are a new action type.** Profiles' `SessionAction` acts on the TV (screen off, power off, …). The reverse direction acts on the PC, so it gets its own `HostAction`: `none`, `lock` (the session), `suspend` (the system), plus `tv_power_off` for the remote's power key, where turning the TV off is a natural choice.
- **TV-to-PC events live in the user profile and run in the session service.** `on_tv_standby`, `on_tv_wake` and `on_remote_power`, each a `HostAction`, defaulting to `none`. The session service runs in the user's session, can lock it and can suspend the system through logind without extra polkit rules, and already mirrors every TV's `state/power`.
- **A cause stamp keeps TisVlay's own power changes from firing them.** The TV's standby broadcast is identical whether its remote or TisVlay caused it (Check A), so the world stamps each `state/power` transition with its cause. It knows its last power command per TV. This is a wire change.
- **The remote's power key is its own event.** SIMPLINK's device power-off reaches the daemon as `<User Control Pressed>` Power (`0x40`) addressed to the PC. The kernel queues it to followers whether or not passthrough is on, so the follower monitor sees it either way. It's explicit and never an echo, so it needs no cause stamp.
- **TisVlay gates remote keys; the desktop maps them.** A per-TV system-profile toggle, "send the remote's keys to the desktop", sets the passthrough flag on the claim. The kernel keymap and the desktop's shortcuts decide what keys mean. The keymap drops `0x40`, so the power key can't become `KEY_POWER` (which suspended pox in Check C). No per-key table in TisVlay.
- **"The TV may wake this PC" is a system-profile field, applied at each sleep.** Default off. The daemon can't write `/sys` (`ProtectKernelTunables=true`), so at logind's `PrepareForSleep` it starts a root oneshot unit, through a narrow polkit rule allowing only that unit. The unit sets the adapter's USB `power/wakeup` to the profile's value. No prompt, so it works at sleep time.

## Implementation

### 1. Core: the profile fields and `HostAction`

- [ ] `HostAction` (`none`, `lock`, `suspend`, `tv_power_off`) in `tisvlay-core`, beside `SessionAction`. It's `#[non_exhaustive]` per the public API policy.
- [ ] User profile: `on_tv_standby`, `on_tv_wake`, `on_remote_power` (`Option<HostAction>`, inheriting like the other fields; resolved default `none`).
- [ ] System profile: `tv_may_wake_pc` and `remote_keys_to_desktop` (`Option<bool>`, resolved default `false`).
- [ ] The GUI's profile panes and the CLI's profile verbs pick the fields up; the docgen profile-defaults partial regenerates.

### 2. World: the cause stamp and the remote power event (wire)

- [ ] `state/power` gains `cause`: `requested` when the transition follows a power command TisVlay sent the TV within a window, `external` otherwise. Covers dispatches, rpcs, wake dials and screen actions emulated as power.
- [ ] A remote power press reaches the world from the CEC connection's follower monitor and is published as a per-TV event key (`event/remote-power`), which the session service subscribes to.
- [ ] Both are wire-visible, in one `feat` commit with the SDK surface.

### 3. Session service: run the host actions

- [ ] A `state/power` transition to standby with `cause: external` runs `on_tv_standby`; to on with `cause: external` runs `on_tv_wake`; `event/remote-power` runs `on_remote_power`.
- [ ] `lock` locks the user's graphical session through logind; `suspend` calls logind's `Suspend`; `tv_power_off` sends the TV a power-off.
- [ ] Gated like the session's other actions: the active user only, and `automate_when` respected.
- [ ] Guard against loops: an `on_tv_standby` suspend leads to a system sleep, whose own system-profile action may power the TV off; the stamp marks that `requested`, so it can't chain.

### 4. Daemon: the remote-keys gate

- [ ] A linux-cec setter for the log-address flags (`CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`). Upstream first (holo/linux-cec on gitlab.steamos.cloud), alongside the pending OSD-name issue ([docs/bugs/upstream-linux-cec-reclaim-drops-osd-name.md](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md)). Until it lands, a vendored patch or a narrow ioctl. Open question below.
- [ ] The CEC link sets the flag on the claim when the TV's resolved system profile says so; a change re-claims.
- [ ] The keymap override dropping `0x40`: shipped by the NixOS module (a hwdb or `rc_keymaps` entry for the `rc-cec` map) and documented for other distros.

### 5. The wake helper

- [ ] A root oneshot `tisvlay-wake@.service` that resolves a CEC node to its USB adapter (`/sys/class/cec/cecN/device` → serio → tty → USB device) and writes the requested `power/wakeup`, reading the value from a file the daemon writes under its state or runtime directory.
- [ ] A polkit rule letting the daemon's user start only that unit, with no prompt.
- [ ] At `PrepareForSleep(true)`, the daemon writes each CEC TV's resolved `tv_may_wake_pc` and starts the unit, before the sleep proceeds (the daemon's logind inhibitor window already exists for the sleep dispatch).
- [ ] The NixOS module installs the unit and the rule; packaging (`.deb`/`.rpm`/Arch) ships them too.
- [ ] Adapters without USB remote wakeup (DisplayPort-AUX CEC on amdgpu/i915) report it, and the field has no effect on them.

### 6. GUI and docs

- [ ] The TV pane shows, read-only, whether the TV can wake this PC (the field's value and the adapter's capability). Open question below.
- [ ] Docs: architecture.md (the reverse direction, the cause stamp), protocol.md (remote keys, SIMPLINK power-off), the packaging notes (the helper, the polkit rule, the keymap). The user guide waits until the CEC follow-ups near their end.

## Verification

- [ ] World and session tests:
  - the cause stamp across dispatches, rpcs, wake dials and emulated screen actions;
  - each host action fired by its event, gated and not chaining;
  - the remote power event.
- [ ] Hardware, reusing the idea doc's checks:
  - A, turned into a test of `on_tv_standby` / `on_tv_wake` with the stamp;
  - B′, the wake with the field on and off, applied by the helper;
  - C, keys with the gate on and the power key dropped.
  These checks run as unattended blocks, since the TV is pox's display.

## Open questions

- **The linux-cec flag setter:** upstream only, or a vendored patch until it lands?
- **The cause stamp's window:** a fixed window after a TisVlay power command, or a stamp carried until the TV's next reported state?
- **Lock versus suspend semantics:** does `suspend` lock first (logind does by default when the desktop is configured to)?
- **Surfacing:** should the GUI show whether the adapter can wake this PC?
- **The label across long armed suspends:** this decides whether the helper should also disarm the Pulse-Eight on suspend (the module parameter).
