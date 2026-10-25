# The TV controls the PC

TisVlay drives the TV from the PC. This plan adds the reverse direction. All of it is opt-in and off by default:
- the TV powering off can lock the session or put the PC to sleep;
- on HDMI-CEC, the remote's "turn this device off" can act on the PC, the TV can wake a sleeping PC, and the remote's keys can reach the desktop.

The decisions come from [docs/ideas/tv-controls-pc.md](../ideas/tv-controls-pc.md): its hardware checks ([A](../ideas/tv-controls-pc.md#checks-to-run): the TV's own power key; B and B′: the TV waking the PC; C: the remote's keys) ran on pox, the maintainer's PC, with an LG C3 and a Pulse-Eight USB-CEC adapter. SIMPLINK is LG's name for its CEC features. Names follow [docs/ideas/power-vocabulary.md](../ideas/power-vocabulary.md): the PC sleeps and wakes, the TV powers off and on (its off state is standby), the session locks, activity idles and resumes, and desktop streaming (today's "remote session") is a stream.

It has three parts:
- **Prerequisites:** two fixes and a rename the rest depends on.
- **A transport-agnostic core:** the TV powering off, detected as a live edge with TisVlay's own commands left out, acting on the PC. It works for CEC and LG over the network alike.
- **CEC extras:** the remote, and waking the PC from sleep. These exist only where the TV is on the CEC bus, and the backend declares them as capabilities.

It builds on #212 (device availability, held wakes, and `88a3d854`'s inputattach restart) and #211's Pulse-Eight fix.

## What each backend can do

| Feature | HDMI-CEC | LG over the network (SSAP) |
|---|---|---|
| The TV powered off | The TV's `<Standby>` / standby power-status broadcasts | The power-state subscription (`Active Standby`, `Suspend`) before the socket closes, **only while TisVlay holds a connection**: a network TV that goes down stays down until asked, so one turned back on with its own remote isn't reconnected, and its next power-off is missed |
| The remote's power key, aimed at the PC | SIMPLINK's device power-off, as `<User Control Pressed>` Power | None |
| The remote's other keys to the desktop | The kernel's `rc-cec` with passthrough (Check C) | None without an app on the TV (idea doc, LG remote keys) |
| The TV wakes a sleeping PC | A firmware adapter (Pulse-Eight) with USB wakeup (Check B′) | None: the TV sends no Wake-on-LAN to its sources |

## Where the settings live

Two questions place every setting (decided 2026-10-25):
1. **Is it a fact or a choice?** A fact about the setup (which adapter, which input is the PC) goes in the TV's device config. A choice about behavior goes in a profile, never as a device-config bool.
2. **For a choice, whose scope does the action touch?** An action on the machine (putting it to sleep, letting the TV wake it, what the shared adapter forwards) goes in the system profile and runs in the daemon, so it works with nobody logged in and a shared machine doesn't depend on one user's settings. An action on the user's session (locking it) goes in the user profile and runs in the session service, for the active user.

What a backend can honor is neither: it's a capability the backend declares, the way `INPUT` already decides whether `restore_input` does anything. The profile fields stay backend-neutral, and a field a TV's backend can't honor does nothing for that TV.

| Setting | Home | Field and type | Needs |
|---|---|---|---|
| The TV powers off → lock | User profile | `on_tv_power_off`: `UserLockAction` | `state/power` (every backend) |
| The TV powers off → sleep | System profile | `on_tv_power_off`: `SystemSleepAction` | `state/power` (every backend) |
| The remote's power key → lock | User profile | `on_remote_power`: `UserLockAction` | `REMOTE_POWER` |
| The remote's power key → sleep | System profile | `on_remote_power`: `SystemSleepAction` | `REMOTE_POWER` |
| The TV powers on → wake the PC | System profile | `on_tv_power_on`: `SystemWakeAction` | `WAKES_PC` |
| The remote's keys reach the desktop | System profile | `remote_keys`: `bool` | `REMOTE_KEYS` |
| The power key never becomes `KEY_POWER` | Host config | the `rc-cec` keymap | none (static, not TisVlay's) |

"Remote" names the TV's remote control here, which is safe once the stream rename (step 3) has freed the word.

Two existing CEC device-config fields predate the rule and are choices: `emulate_screen_via_power` and `advertise_osd_name`. They stay as they are in this plan; the idea doc tracks moving them. `osd_name` is a fact (what this PC is called on the bus) and stays.

## Decisions

### Core (every backend)

- **PC actions are typed by scope and direction.** Profiles' existing action, `SessionAction`, acts on the TV (screen off, power off, …). The reverse direction acts on the PC, with one type per scope and direction, named after what it does:
  - `UserLockAction` (`none`, `lock`), for the user profile's going-off events;
  - `SystemSleepAction` (`none`, `suspend`, `sleep`), for the system profile's going-off events;
  - `SystemWakeAction` (`none`, `wake`), for the system profile's `on_tv_power_on` (CEC extras).

  The going-off events are the TV powering off and, on CEC, the remote's power key. A user profile can't express a sleep, and `on_tv_power_off` can't be set to wake. The enums are exhaustive, like `SessionAction` (docs/build.md § Public API policy): every arm matters to the dispatcher, so a new one should fail the build until it's handled.
- **The sleep mode is the action's value.** `suspend` calls logind's `Suspend()`: always suspend to RAM, the state the TV's wake of the PC was tested from, and what Plasma's own "Sleep" does by default. `sleep` calls logind's `Sleep()`, which picks the host's configured mode from `SleepOperation=` (like `systemctl sleep`). That's the machine-scope equivalent of the desktop's own sleep setting, which is per user and absent at the login screen. On a host with hibernation it can be suspend-then-hibernate, as on pox, which hibernates after 2 h without a battery. `Sleep()` and `CanSleep` exist from systemd 256; on older hosts (Debian 12, RHEL 9 and Ubuntu 24.04 ship 252 to 255) the `sleep` value is unavailable. The daemon asks logind's `CanSuspend` / `CanSleep` itself and publishes the answers: logind answers for the caller's polkit rights, so only the daemon's answer says what the daemon may do, and a missing polkit rule shows up there.
- **What counts as the TV powering off.** Power states fall in three classes:
  - **on:** `Active`, `ScreenSaver`, `ScreenOff` (today's `is_awake`);
  - **off:** `Standby`, `ActiveStandby`, `Suspend`;
  - **transient:** `WarmingUp`, `CoolingDown`, `Unknown`, and any future variant.

  The event fires only on a **live edge** from on to off: between two reports on the same connection. A connection's first reading (the `Connected` snapshot, CEC's seed reading, and its `Active` fallback when the query fails) only sets the baseline. Without that, a keyboard wake with the TV in standby would reconnect CEC, read standby against the cached on, and put the PC straight back to sleep, on every wake. Transient states never fire and never count as the edge's end. LG's parser maps any `processing` value to `ActiveStandby` for the wake pre-flight, so the classifier reads LG's settled state rather than that mapping (checked on hardware during screen off and on).
- **The world detects the edge and publishes it.** On a live edge not caused by TisVlay, the world runs the system profile's action and publishes `event/tv-power-off`: site-level like the other events, with the TV in the payload. The session subscribes without history and ignores a sample whose zenoh timestamp is more than 5 s old, so a login, a restart or a link gap can't replay an old power-off into a lock. `state/power` doesn't change.
- **A cause stamp keeps TisVlay's own power changes out.** The TV reports the same transition whether its remote or TisVlay caused it (on CEC, identical broadcasts, Check A). The stamp is world-internal:
  - The backend bridge (`backend.rs`), which every command passes through, tells the world it's sending a power-off or a blank just before it does, from the same task as the TV's reports, so the expectation lands first. A blank counts too: LG's parser may read a screen-off's `processing` as Active Standby, and a missed real power-off within 60 s is the safe side of that trade. Power-ons record nothing yet, since no coming-on edge fires anything.
  - The first live off report consumes it, and that edge is TisVlay's own: it fires nothing. A transient report doesn't consume it.
  - Nothing is recorded when the TV's last settled class is already off: the plan's own chains send power-offs to TVs already off (a lock's screen-off emulated as power, `on_sleep`).
  - An expectation expires after 60 s, the held wake's bound, and a newer one renews it. All expectations clear when the PC wakes, because the world's clock (a monotonic `Instant`) stops during sleep.
  - The tracking lives in a world resource keyed by TV, not in components, so one batch's reports see each other at once.
- **Each scope runs its own, behind the existing gates.** The session service runs the user profile's PC actions: it runs in the user's session, can lock it, and gates on the active user and `automate_when`. The daemon's world runs the system profile's, gated on `managed` and `automate_when`, and puts the PC to sleep through logind, which needs a polkit rule for the daemon's user since the daemon runs outside any session. That way a sleep works at the login screen and with nobody logged in.
- **PC actions never fire while a stream arms the TV.** A fixed gate, with no setting, in both scopes: while the TV is in the world's `StreamArmed` set (mirrored from the session), an eco-timer power-off or a press of the remote in the room would otherwise put the PC to sleep or lock the session under the streaming user. The session runs its Sunshine refresh (`eager_refresh_sunshine`) before deciding, as it does for its other gated events.
- **The sleep handoff holds until the TV commands are sent.** Today the daemon releases logind's delay inhibitor right after queuing the sleep event (`crates/tisvlay-daemon/src/logind.rs`), so `on_sleep`'s TV power-off races the suspend. A PC-action sleep depends on that power-off landing (the TV follows the PC through `on_sleep`), so the daemon holds the inhibitor until the sleep dispatch's sends complete, bounded by logind's `InhibitDelayMaxSec` (5 s by default).
- **The polkit rule grants sleeping, not overriding.** logind's suspend and hibernate actions (`sleep` may pick either), with their multiple-sessions variants, which are always needed since the daemon's user has no session of its own. Not the ignore-inhibit actions: a user's block inhibitor wins, and a refused call lands on `event/dispatch-failed`. The NixOS module and the `.deb`/`.rpm`/Arch packages always ship it, from one source file: the profile field, default `none`, is the opt-in, and a second switch at install time would be one a GUI user can't see. The cost is that whoever may edit system profiles (the `tisvlay` group) can have the machine sleep.
- **Lock and sleep compose.** Setting both runs both, each from its own service. Their order isn't guaranteed, so a lock that must land before the sleep comes from the desktop's own lock-on-sleep setting.
- **Each TV acts alone.** No guard across TVs: one TV powering off acts even while another TV still shows the PC. Per-TV profile assignment is the control. The idea doc tracks a guard.
- **The TV powering on doesn't act on an awake PC.** The user profile has no `on_tv_power_on`. The reaction that would help, waking the desktop out of DPMS off so the TV shows the lock screen, waits in the idea doc with the input-change events.

### CEC extras

- **Backend support is a capability, computed by the CEC connection.** Capabilities come only from `TvConnection::capabilities()` at connect, so the connection computes these from the adapter it claimed:
  - `REMOTE_POWER`: the remote's power key reaches the PC. Every CEC connection has it.
  - `REMOTE_KEYS`: the remote's keys can reach the desktop: the adapter has the kernel's `CEC_CAP_RC` (linux-cec's `Capabilities::RC`).
  - `WAKES_PC`: the TV can wake this PC: the adapter's first USB-device ancestor (from `/sys/bus/cec/devices/cecN`) has a `power/wakeup` file. Its presence counts, not its value, which the wake unit toggles. A DisplayPort-AUX adapter has no USB ancestor, and a walk to just any ancestor with `power/wakeup` would wrongly find the GPU's.

  The world skips a field for a TV without its capability, and the GUI shows them on the TV pane, so a user can see why a setting does nothing there. Adding flags is a wire change. Screen-via-power isn't a precedent here: it's a config-derived transport trait, and two comments in `crates/tisvlay-cec/src/connection.rs` still claim the daemon adds `SCREEN`, which it no longer does.
- **The remote's power key is its own event.** SIMPLINK's device power-off reaches the daemon as `<User Control Pressed>` Power (`0x40`) addressed to the PC. The kernel queues it to followers whether or not passthrough is on, by a reading of the kernel source that a hardware check confirms (Check C ran with `cec-ctl`'s passthrough claim, not TisVlay's). The monitor discards key presses today; it keeps this one, debounced, since a held key repeats. It becomes `on_remote_power` in both profiles, with the same types as `on_tv_power_off`, and `event/remote-power` like `event/tv-power-off`. It's explicit and never an echo, so it needs no cause stamp. The TV follows the PC through the events it already has: a sleep runs the system profile's `on_sleep`, which powers the TV off by default, and a lock runs the user profile's `on_session_lock`.
- **TisVlay gates the remote's keys; the desktop maps them.** A system-profile toggle, `remote_keys`, sets the passthrough flag on the claim, after clearing it, as the OSD name is set (the kernel's clear wipes linux-cec's copy). The kernel keymap and the desktop's shortcuts decide what keys mean, with no per-key table in TisVlay. The keymap drops `0x40`, so the power key can't become `KEY_POWER` (which put pox to sleep in Check C); it ships in every package, since the toggle exists everywhere and the entry is harmless while passthrough is off.
- **The TV waking the PC is the system profile's `on_tv_power_on = "wake"`, armed for every sleep.** Default `none`. Nothing in TisVlay runs while the PC sleeps, and the daemon can't write `/sys` (`ProtectKernelTunables=true`), so a static root unit applies it:
  - `tisvlay-wakeup.service` is `WantedBy=` and `Before=sleep.target`, with `DefaultDependencies=no`, and runs a hidden `tisvlay` subcommand.
  - The subcommand reads `/run/tisvlay/wakeup`, a list of CEC nodes the world rewrites (an effect like `Persist`) whenever the set of TVs resolving to `wake` changes, resolves each node to its USB device, and enables its `power/wakeup`.
  - It only undoes what it enabled: it records the devices it enabled under its own runtime directory and disables those no longer listed. An adapter it never enabled stays as the host configured it, so the default `none` never overrides a user's own udev rule.
  - With the daemon stopped, `/run/tisvlay` is gone, the list is empty, and the unit disables only what it had enabled.
  - No polkit rule is needed to start it, and no timing depends on the daemon.

  Among the Pulse-Eight's documented wake triggers is Set Stream Path to its own address, the TV coming up on the PC's input; a TV coming up on another input doesn't trigger that. LG's vendor command `0x03 0x00` is also a trigger, though, and if it wakes the PC while the TV comes up on the Chromecast, the system profile's `on_wake` (stock: power on, Image View On over CEC) would take the TV's input from the Chromecast; a hardware check decides whether that needs handling. Whether the TV can wake a hibernated PC is under test. Disarming the Pulse-Eight on sleep stays host configuration ([cec-pulse8-autonomous.md](cec-pulse8-autonomous.md)); no long sleep has pinned the "LG DVD Player" label so far (2026-10-25).

### Public API

- **tisvlay-core:**
  - `UserLockAction`, `SystemSleepAction` and `SystemWakeAction` beside `SessionAction`, exhaustive.
  - The new `SystemProfileConfig` / `UserProfileConfig` fields. New public fields break struct-literal construction, which a minor bump allows before 1.0; cargo-semver-checks reports them from the second release.
  - `Capabilities::REMOTE_POWER`, `REMOTE_KEYS` and `WAKES_PC`, with the hand-written `to_strings` / `from_strings` lists in `capabilities.rs` matching `iter_names`' output.
  - The power-state classes beside `PowerState::is_awake`. They become the one classification: the vocabulary's renames retire `is_on` and `is_awake` for them.
  - Two `TvConnection` additions with defaults, so the LG and mock backends are untouched: a path for the remote's power key alongside `subscribe_state` (which carries only `TvState`), and a setter for the passthrough flag (`TvConnector::connect` takes only the vendor config, and the flag is profile policy that can change while connected).
- **tisvlay-protocol:** the `event/tv-power-off` and `event/remote-power` payloads and their `keys::event` names, and the daemon's published `CanSuspend` / `CanSleep` answers. `Power` (the `state/power` payload) is unchanged.
- **The SDK:** nothing required. Capabilities already travel as strings, and the GUI reads profile bodies as wire payloads.

## Implementation

Every slice ships whole:
- **Its GUI rows**, in a card of their own, as `USER_WHILE_STREAMING_FIELDS` is. The parity tests (`system_field_table_matches_core_schema`, `user_field_table_matches_core_schema`) fail on a core field without a GUI row. The three new enums need options, parsing and a `BodyPatch` arm each; parse their tokens through serde. Rows need a help slot, which `ProfileFieldEntry` lacks today, and the sleep values the daemon reports unavailable show as such, since the ComboBox has no per-option disabled state.
- **Its generated docs** (`just docs-gen`). The profile-defaults partials publish field doc comments in the user guide at once, so they're written for users.
- **Its `.pot` refresh** (`just i18n-extract`), and its tests.

(wire) marks a `feat` or breaking commit. Profile bodies are wire payloads, so every slice that adds a profile field is one.

### Prerequisites

#### 1. The GUI's power-state labels

- [x] `tvs_pane.slint` compares `power-state` against `"warming_up"` and `"cooling_down"`, but the wire tokens are serde's lowercase `"warmingup"` and `"coolingdown"`, so "Warming up…" and "Cooling down…" never show and the pane prints "Power state: warmingup". Key the labels on the wire tokens, with a test pinning every token the GUI matches against `PowerState`'s serialization (the drift [docs/bugs/power-state-vocabulary-drift.md](../bugs/power-state-vocabulary-drift.md) closed for `is_on`).

#### 2. The sleep handoff

- [x] The daemon holds logind's delay inhibitor until the sleep (or shutdown) dispatch's sends complete, bounded by `InhibitDelayMaxSec`, instead of releasing it right after queuing the event. The logind watcher sends the world a drain token after the event. The reactor queues it on every live connection behind that step's commands, and each connection drops it once it has run them, so the watcher releases when the last copy is gone. `FakeLogind` hands out real pipes and records each release; the tests pin the hold behind a slow power-off and the release at logind's bound.

#### 3. Remote sessions become streams (breaking)

- [x] The rename in [docs/ideas/power-vocabulary.md](../ideas/power-vocabulary.md#streams): config keys, the `remote/**` and `user/remote/**` keys, the `remote-session` CLI verb, the GUI card, docs. One breaking commit, before steps 6 and 7 take the word "remote".

### Core

#### 4. The TV powering off, and the lock (wire)

- [x] The power-state classifier (`PowerState::class`, `PowerClass`), the live-edge detection in the world's fact systems (first readings set the baseline), and the cause stamp from the backend bridge (`BackendEvent::SendingOff`).
- [x] `event/tv-power-off` (`keys::tv_power_off`, `payload::TvPowerOff`), published for a live edge TisVlay didn't cause.
- [x] `UserLockAction` and the user profile's `on_tv_power_off`, with its GUI row in the main card for now; a card of its own fits once `on_remote_power` joins it. The session subscribes to the event without history, drops stale samples, applies its gates and the stream gate, and sends a lock request to `spawn_locker`. The locker asks logind to lock the user's seat session (`Session.Lock`, which the session's own user may call without polkit).

#### 5. The sleep (wire)

- [x] `SystemSleepAction` and the system profile's `on_tv_power_off`, with its GUI row. On the same edge, the world resolves the TV's sleep, gates it on `managed`, `automated` and the stream gate, and emits a `SleepRequest` (its `SleepMode` has no `none`). The daemon's sleeper calls logind's `Suspend(false)` or `Sleep(0)`. A refused call, `Sleep`'s `UnknownMethod` on systemd before 256 included, comes back as `InEvent::SleepRefused` and lands on `event/dispatch-failed`.
- [x] The polkit rule, from one source file (`packaging/polkit/50-tisvlay.rules`), in the NixOS module (for its configured user) and every package; the NixOS VM test checks it. It takes effect where polkit runs: every desktop does, and the module doesn't turn polkit on for a headless host.
- [x] Loops can't form: a sleep runs `on_sleep`, whose power-off the stamp marks as TisVlay's own.
- [ ] The daemon puts its `CanSuspend` / `CanSleep` answers on the `manager/rpc/info` reply, which the GUI already fetches per lifetime (decided 2026-10-25; no new key), and the system pane marks the values the host can't take.

### CEC extras

#### 6. The remote's power key (wire)

- [ ] `REMOTE_POWER`, the `TvConnection` path for the key, the CEC monitor keeping `<User Control Pressed>` Power addressed to us (debounced), and a `BackendEvent` variant with its arm in the bridge.
- [ ] `event/remote-power` and `on_remote_power` in both profiles, run like `on_tv_power_off`.

#### 7. The remote's keys (wire)

- [ ] A linux-cec setter for the log-address flags (`CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`), on a branch of our fork ([TisVlay/linux-cec](https://github.com/TisVlay/linux-cec)) based on the `linux-cec-0.2.1` tag, so the swap changes nothing else.
  - The workspace's `[patch.crates-io]` points `linux-cec` at that branch by `git` and `rev`, with no submodule.
  - `deny.toml` gains an `allow-git` entry for the fork, and the Nix package a `cargoLock.outputHashes` entry.
  - The same change goes upstream as a merge request (holo/linux-cec on gitlab.steamos.cloud), beside the drafted OSD-name issue ([docs/bugs/upstream-linux-cec-reclaim-drops-osd-name.md](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md)). The `[patch]` comes out once a release carries it.
  - Not a direct ioctl: linux-cec keeps its file handle private, and a claim made beside it would leave linux-cec's own record of the claim stale. linux-cec uses that record to pick the sender address for every message it sends.
  - Its retirement gets a docs/build.md review trigger. The notes that say linux-cec comes from crates.io (`Cargo.toml`, `docs/licensing.md`, `deny.toml`) name the fork, its modified files carry LGPL-2.1's change notices, and Dependabot bumps to the `rev` must move the Nix `outputHashes` with them. `just sbom-check` compares `name@version` only, so it can't tell the fork from crates.io's 0.2.1.
- [ ] `REMOTE_KEYS`, the system profile's `remote_keys` (`Option<bool>`, resolved default `false`), and the `TvConnection` setter. The CEC link sets the flag after clearing the claim when it's on and the TV has `REMOTE_KEYS`; a change re-claims.
- [ ] The keymap override dropping `0x40` (a hwdb entry for the `rc-cec` map), in every package, in this commit or earlier.

#### 8. The TV wakes the PC (wire)

- [ ] `WAKES_PC`, from one Rust resolver walking from `/sys/bus/cec/devices/cecN` to the first USB-device ancestor, behind a sysfs-root seam (the DPMS watchers already take `/sys/class/drm` as a parameter). It gets the outside-input parser lint profile and a never-panic proptest (docs/build.md lists sysfs data).
- [ ] `SystemWakeAction` and the system profile's `on_tv_power_on` (`Option<SystemWakeAction>`, resolved default `none`).
- [ ] The world's `/run/tisvlay/wakeup` effect, the hidden subcommand, and `tisvlay-wakeup.service`, in the NixOS module and every package.

### 9. Docs at merge

- [ ] architecture.md: the placement rule from [Where the settings live](#where-the-settings-live), PC actions, the live edge and the cause stamp, the sleep handoff, the stream gate, and the key-space entries for the two events.
- [ ] docs/cec/protocol.md: the remote's keys, SIMPLINK's power-off and the wake triggers; the Checks A to C records move under docs/cec/.
- [ ] packaging/README.md: the polkit rule, the wakeup unit and the keymap.
- [ ] The glossary moves to its home (architecture.md and a user-guide reference page), the idea doc is trimmed to what stays unplanned, and its index entry with it. The user guide's prose chapter waits until the CEC follow-ups near their end.

## Verification

- [ ] Without hardware (world, session and daemon tests):
  - the classifier over every `PowerState`, and live edges against first readings (including CEC's `Active` fallback);
  - the cause stamp across dispatches, rpcs, power-on dials, held wakes and emulated screen actions; a command to a TV already off; a transient report; expectations cleared at wake;
  - a replayed or stale `event/tv-power-off` ignored by the session at login and after a restart;
  - the stream gate in both scopes;
  - `FakeLogind` gaining `Suspend`, `Sleep`, `CanSuspend`, `CanSleep` and `Session.Lock`: each PC action fired by its event and gated, a refused sleep on `event/dispatch-failed`, and the inhibitor held until the sends complete;
  - `FakeCecLink`: the remote's power key as an event, debounced, and the passthrough flag set after the claim is cleared;
  - sysfs fixtures: `WAKES_PC` for a USB adapter and not for a DP-AUX one under a GPU, the resolver never panicking, and the unit enabling what's listed and disabling only what it enabled;
  - GUI tests for the new rows; the VM test for the polkit rule.
- [ ] Hardware, on the LG C3, run as unattended blocks since the TV is pox's display:
  - `on_tv_power_off` over CEC (Check A's sequence) and over SSAP, with TisVlay's own power-offs left out;
  - what LG reports during a screen off and on (its `processing` values);
  - a keyboard or Wake-on-LAN wake with the TV in standby, which must not put the PC back to sleep;
  - a system-profile sleep at the login screen, with nobody logged in;
  - `on_remote_power` with passthrough off, the default;
  - toggling `remote_keys` (a re-claim) firing nothing, and Check C with the gate on and the power key dropped;
  - Check B′ with `on_tv_power_on` set to `wake` and to `none`, through the unit; a PC-action sleep with the wake armed staying asleep until the TV comes on; the TV powering on to another input with the wake armed;
  - the TV waking a hibernated PC (the check in progress);
  - an LG turned on with its own remote and then off, which the table above says goes unseen.
