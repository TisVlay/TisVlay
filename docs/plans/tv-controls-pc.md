# The TV controls the PC

TisVlay drives the TV from the PC. This plan adds the reverse direction. All of it is opt-in and off by default:
- the TV powering off can lock the session or put the PC to sleep;
- on HDMI-CEC, the remote's "turn this device off" can act on the PC, the TV can wake a sleeping PC, and the remote's keys can reach the desktop.

The decisions come from [docs/ideas/tv-controls-pc.md](../ideas/tv-controls-pc.md): its hardware checks ([A](../cec/checks.md): the TV's own power key; B and B′: the TV waking the PC; C: the remote's keys) ran on pox, the maintainer's PC, with an LG C3 and a Pulse-Eight USB-CEC adapter. SIMPLINK is LG's name for its CEC features. Names follow [docs/ideas/power-vocabulary.md](../ideas/power-vocabulary.md): the PC sleeps and wakes, the TV powers off and on (its off state is standby), the session locks, activity idles and resumes, and desktop streaming (today's "remote session") is a stream.

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
- **The remote's power key is its own event.** SIMPLINK's device power-off reaches the daemon as `<User Control Pressed>` Power (`0x40`) addressed to the PC. The kernel queues it to followers whether or not passthrough is on, by a reading of the kernel source that a hardware check confirms (Check C ran with `cec-ctl`'s passthrough claim, not TisVlay's). The monitor discards key presses today; it keeps this one, debounced, since a held key repeats: a press fires once, and further presses are ignored until `<User Control Released>` or 1 s without one. It becomes `on_remote_power` in both profiles, with the same types as `on_tv_power_off`, and `event/remote-power` like `event/tv-power-off`. It's explicit and never an echo, so it needs no cause stamp, and it passes only the stream gate (decided 2026-10-25): `managed` and `automate_when` keep TisVlay from acting on the TV by itself, which a press isn't, and `automate_when` reads an input CEC doesn't report. The TV's own follow-up (`on_sleep`'s power-off) still passes them. The TV follows the PC through the events it already has: a sleep runs the system profile's `on_sleep`, which powers the TV off by default, and a lock runs the user profile's `on_session_lock`.
- **TisVlay gates the remote's keys; the desktop maps them.** A system-profile toggle, `remote_keys`, sets the passthrough flag on the claim, after clearing it, as the OSD name is set (the kernel's clear wipes linux-cec's copy). The kernel keymap and the desktop's shortcuts decide what keys mean, with no per-key table in TisVlay. The keymap drops the four power keys, so none can become `KEY_POWER` (which put pox to sleep in Check C) or another power key; it ships in every package, since the toggle exists everywhere and the entry is harmless while passthrough is off.
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
  - Two `TvConnection` additions with defaults, so the LG backend is untouched: `subscribe_events`, a stream of a `#[non_exhaustive]` `TvEvent` (`RemotePower` for now) alongside `subscribe_state`, which carries only `TvState` snapshots (decided 2026-10-25; its end means stop listening, not a disconnect, and CEC's one monitor task feeds both), and a setter for the passthrough flag (`TvConnector::connect` takes only the vendor config, and the flag is profile policy that can change while connected).
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
- [x] The daemon asks logind's `CanSuspend` / `CanSleep` once at startup and puts the answers on the `manager/rpc/info` reply (`ServiceInfo::sleep`, a `SleepSupport`), which the GUI already reads per lifetime (decided 2026-10-25; no new key). The system sleep row offers only the granted modes, keeping a saved choice that's no longer granted, since the ComboBox has no per-option disabled state.

### CEC extras

#### 6. The remote's power key (wire)

- [x] `REMOTE_POWER`, `TvEvent` and `subscribe_events`, the CEC monitor keeping `<User Control Pressed>` Power addressed to us (debounced), and a `BackendEvent` variant with its arm in the bridge. The mock implements the stream and advertises the capability, with `MockHandle::press_remote_power` driving the world and session tests. The monitor also reads the Power Off function (`0x6C`) as the power key, since a TV of another make may send it for the same menu item, but not Power Toggle (`0x6B`), which a TV may send to wake the PC.
- [x] `event/remote-power` and `on_remote_power` in both profiles, run like `on_tv_power_off` but behind the stream gate only. Both going-off fields move to a "From the TV" card in each profile editor, and the TV pane's "From the TV" group says whether the TV's connection has `REMOTE_POWER`.

#### 6b. The TV's wake press

- [x] **Found on hardware (2026-10-25, pox, LG C3).** The TV also sends the PC `<User Control Pressed>` Power when it switches to the PC's input to wake it, and step 6 reads that as the remote's power key. With `on_remote_power = "suspend"`, that would put an awake PC to sleep at the moment the TV selects it. Seen once, at 22:10:39 in check 2: the TV reported itself on (`0f 90 00`) after TisVlay's Wake-on-LAN power-on, sent `08 44 40` / `08 45` 1.07 s later, then Set Stream Path to the PC (`0f 86 10 00`) 0.19 s after the press. A manual run then mapped the TV's behavior from the kernel's CEC log (`cec` `debug=2`), with every PC action set to `none`:

  | Action | Frames | A press at the PC? |
  |---|---|---|
  | The TV's own remote, off then on, showing the PC | `0f 36`, `0f 90 01`; then `0f 90 00`, `0f 86 10 00` | no |
  | Wake-on-LAN from Active Standby, and TisVlay's power-off then its power-on (5 s and 20 s off) | `0f 90 00`, `0f 86 10 00` | no (3 tries) |
  | TisVlay's CEC power-on (Image View On, Active Source from the PC) | `80 04`, `8f 82 10 00`; `0f 90 00` | no |
  | SIMPLINK's "power off this device", TV showing the PC | `08 44 40`, `08 45`, nothing after; the TV stays on | the real press |
  | Input switch to the Chromecast over the network (step 2b) | Routing Change 1.0→4.0, then `04 44 40` / `04 45` to the Chromecast, then Set Stream Path 4.0 | yes, to the device switched to |
  | Input switch with the TV's input button or the SIMPLINK device list | Routing Change only (to the Chromecast, or back to the PC); no press with both devices awake | no |
  | TV on the Chromecast, off, on (comes up there), then to the PC | `0f 86 40 00` at power-on; then Routing Change 4.0→1.0 and Set Stream Path 1.0 | no |
  | Daemon restart with the TV showing the PC, then a SIMPLINK power-off 8 s later | our claim and identity exchange; no routing frame from the TV before the press | the real press |

  Earlier readings corrected by this run: the TV's `<Standby>` broadcast 0.4 s after some presses was its reply to TisVlay's own `<Standby>` (`80 36`, the stock `on_session_lock` screen-off emulated as power, sent within the millisecond of the session's lock), not part of SIMPLINK's power-off; and the press isn't caused by a network power-on as such.
- [x] **Prior art.** libcec fixed the same bug on 2026-09-02 (`efe7d98c`, "fixed: a power toggle switched the device off as the TV selected it", #744). In `CCECCommandHandler::HandleUserControlPressed`, Power and Power Toggle are toggles, and a toggle reads as "off" only while the device is the active source and on: "a TV that is switching to this input sends one to wake the source it is switching to, and switching off at the moment the user asked to watch it is never what was meant." Power Off Function has no power branch there. The HDMI CEC specification isn't public, so libcec is the reference.
- [x] **Proposed rule (superseded by the decision below).** The CEC monitor reads a power-key press as "power off this device" only while the TV has been showing the PC for at least 2 s:
  - **Shown, from now:** a Routing Change or Routing Information to the PC's physical address, a Set Stream Path to it, or an Active Source for it (the PC's own on a CEC power-on included). Each one restarts the 2 s, since a switch can send a Routing Change and then a Set Stream Path.
  - **Not shown:** a route or Active Source for any other address, or the TV's standby.
  - **At connect:** shown and settled if the TV is on (a restart gets no routing frame, row 8), not shown if it's in standby.
  - Against the evidence: the 22:10:39 press came after the TV's standby (not shown: ignored); every SIMPLINK press came minutes after a route to the PC or the PC's own Active Source (acted on); a wake press 0.2 s after a Routing Change to the PC, the step-2b shape aimed at the PC, falls inside the 2 s (ignored). The cost: a SIMPLINK power-off within 2 s of switching to the PC is ignored.
- [x] **Open questions, settled.** Power Toggle (`0x6B`) is a power key again, under the rule. Power Off Function (`0x6C`) stays unconditional. What makes the TV press an *awake* PC remains unknown: it reproduced once in four tries.
- [x] **The network-switch capture (2026-10-31, 10:37).** TisVlay's own input switches over SSAP, with the PC awake and claimed and every power-key action at `none`, to check whether a switch to the PC sends it a press right after the route (the ordering a rule without a settle time would misread):

  | Switch | Frames | A press? |
  |---|---|---|
  | PC (HDMI 1) to the Chromecast (HDMI 4), the Chromecast awake | Routing Change 1.0→4.0, Set Stream Path 4.0, then the Chromecast's own Active Source (`4f 82 40 00`) | no |
  | The Chromecast back to the PC | no routing frame at all; the TV asks the PC's vendor ID (`08 8c`) | no |

  So the step-2b press went to a sleeping Chromecast, a wake; an awake source got none. The Chromecast is the bus's logical address 4, the device polling the TV's power every 8 s. A first try minutes earlier sent no routing frames either: the TV's CEC side was stuck reporting standby with its panel on, after TisVlay's emulated screen-off and screen-on flapped twice at the lock screen (the default user profile's 5 s locked idle timeout), and it ignored Image View On until the TV was power-cycled with its remote.
- [x] **Decision (2026-10-31): libcec's rule, without a settle time.** A Power or Power Toggle press reads as "power off this PC" only while the TV is showing the PC; Power Off Function always does; Power On never does. The capture showed no press after a route to an awake PC, and the one unexplained press (22:10:39) came after the TV's standby, when the PC wasn't shown. The 2 s settle stays the fallback if a press ever slips through.
- [x] As built: the CEC monitor's `Shown` follows the bus from any initiator (a Routing Change or Routing Information, a Set Stream Path, an Active Source: shown while it names the PC's physical address, the PC's own looped-back Active Source included; the TV's `<Standby>` or a standby `<Report Power Status>` shows nothing), starting shown when the TV is on at subscribe. `PowerKey::press` returns the key, Power Toggle joins the power keys, and `powers_off` gates the toggles on `Shown`. The connection takes the configured physical address from the connector.

#### 7. The remote's keys (wire)

- [x] A linux-cec setter for the log-address flags (`CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`), on the `rc-passthru` branch of our fork ([TisVlay/linux-cec](https://github.com/TisVlay/linux-cec)) based on the `linux-cec-0.2.1` tag, so the swap changes nothing else. It follows linux-cec's own precedent (decided 2026-10-25): `LogicalAddressFlags` re-exports linux-cec-sys's `CEC_LOG_ADDRS_FL` as `Capabilities` re-exports `CEC_CAP`, and `set_logical_address_flags` / `get_logical_address_flags` pair like `set_vendor_id` / `get_vendor_id`, the setter storing the flags for the next `set_logical_addresses`. Pushed as `334a097e`; its sources at the tag match crates.io's 0.2.1, 0.1.0 and 0.2.1 releases of the three crates.
  - The workspace's `[patch.crates-io]` points `linux-cec` at that branch by `git` and `rev`, with no submodule.
  - `deny.toml` gains an `allow-git` entry for the fork, and the Nix package a `cargoLock.outputHashes` entry.
  - The same change goes upstream as a merge request (holo/linux-cec on gitlab.steamos.cloud), drafted in [docs/bugs/upstream-linux-cec-log-addr-flags.md](../bugs/upstream-linux-cec-log-addr-flags.md) beside the OSD-name issue ([docs/bugs/upstream-linux-cec-reclaim-drops-osd-name.md](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md)); the maintainer files both after the soak. The `[patch]` comes out once a release carries it.
  - Not a direct ioctl: linux-cec keeps its file handle private, and a claim made beside it would leave linux-cec's own record of the claim stale. linux-cec uses that record to pick the sender address for every message it sends.
  - Its retirement gets a docs/build.md review trigger. The notes that say linux-cec comes from crates.io (`Cargo.toml`, `docs/licensing.md`, `deny.toml`) name the fork, its modified files carry LGPL-2.1's change notices, and Dependabot bumps to the `rev` must move the Nix `outputHashes` with them. `just sbom-check` compares `name@version` only, so it can't tell the fork from crates.io's 0.2.1.
- [x] `REMOTE_KEYS`, the system profile's `remote_keys` (`Option<bool>`, resolved default `false`), and the `TvConnection` setter. The CEC link sets the flag after clearing the claim when it's on and the TV has `REMOTE_KEYS`; a change re-claims. As built: the world's `reconcile_remote_keys` sends `BackendCmd::SetRemoteKeys` to each live connection whose identity carries `remote_keys`, whenever the resolved value differs from what that connection was last told (a new connection starts at `false`, its claim being fresh). The link re-claims only when the flags differ, keeping the claim's OSD name, so a TV with `remote_keys` on re-claims once after each connect. The CEC connection has `REMOTE_KEYS` when its adapter has `CEC_CAP_RC`; the mock records the calls (`MockHandle::remote_keys`). The GUI's FROM THE TV card gets the switch, and the TV pane a "Remote keys" row.
- [x] The keymap override (a hwdb entry for the `rc-cec` map), in every package, in this commit or earlier. It drops the four power keys, Power, Power Toggle, Power Off and Power On (`0x40`, `0x6B` to `0x6D`, each checked against the kernel's `rc-cec.c`), so the PC's power answers only to the profiles (decided 2026-10-25). Where no package installed it (a dev stack, a hand install), passthrough lets the power key reach the desktop: the field's doc comment and packaging/README.md say so, and the daemon doesn't try to detect it, since a compiled hwdb can't be checked reliably. As built: `packaging/udev/70-tisvlay-rc-cec.hwdb` matches `evdev:input:b001E*` (bus `0x1E` is `BUS_CEC`) and sets the four scancodes to `reserved`; pox's live map read `0x40` and `0x6B` as `KEY_POWER`, `0x6C` as `KEY_SLEEP` and `0x6D` as `KEY_WAKEUP`. It compiles under `systemd-hwdb --strict` and matches pox's `rc-cec` modalias. nfpm installs it in `/usr/lib/udev/hwdb.d/`, the module in `services.udev.extraHwdb`, and the VM test queries it.

#### 8. The TV wakes the PC (wire)

- [x] `WAKES_PC`, from one Rust resolver walking from `/sys/bus/cec/devices/cecN` to the first USB-device ancestor, behind a sysfs-root seam (the DPMS watchers already take `/sys/class/drm` as a parameter). It gets the outside-input parser lint profile and a never-panic proptest (docs/build.md lists sysfs data). As built: `tisvlay_cec::wakeup` (`cec_node` follows a udev alias to `cecN` and accepts nothing else, `usb_wakeup_device` takes the first ancestor whose `uevent` says `DEVTYPE=usb_device`), with fixtures for pox's Pulse-Eight (whose xHCI controller's own `power/wakeup` must not be found) and a DP-AUX adapter under a GPU; the connector reads it at connect.
- [x] `SystemWakeAction` and the system profile's `on_tv_power_on` (`Option<SystemWakeAction>`, resolved default `none`).
- [x] The world's `/run/tisvlay/wakeup` effect, the hidden subcommand, and `tisvlay-wakeup.service`, in the NixOS module and every package. As built: `TransportFacts` carries the connector's local device node, and `publish_wake_list` sends the sorted nodes of TVs resolving to `wake` whenever the set changes (the first step sends the empty list, replacing a previous daemon's). It doesn't wait for `WAKES_PC`: the unit runs the same resolver and skips what it can't arm, so a list made before a TV's first connect after a restart still holds. The daemon writes it beside its socket (`/run/tisvlay/wakeup` when packaged); `tisvlay service wakeup` (hidden) arms, records in `/run/tisvlay-wakeup/enabled` (`RuntimeDirectoryPreserve=yes`, so until reboot, when the kernel forgets the setting too) and disarms only what it armed. The GUI's FROM THE TV card gets the row and the TV pane a "Wakes this PC" row; the VM test checks the unit is wanted by `sleep.target` and runs.

#### 8b. The adapter's EEPROM decides the wake (found on hardware)

- [x] **Finding (2026-10-30, pox, Pulse-Eight, LG C3).** With `on_tv_power_on = "none"`, the wakeup unit disabled the adapter's `power/wakeup` before the sleep as designed, and the TV powering on still woke the PC: three times, once with every other bus-1 USB wake source disabled and once with the GPU's PCIe port (PEG1) disabled too. With the adapter unplugged during the sleep, the TV powering on didn't wake the PC, and the keyboard did. So the adapter wakes the PC whatever its `power/wakeup` says: with it disabled, Linux doesn't enable the device's remote wakeup at suspend, so the adapter signals wakeup without being asked to. Check B′ (2026-10-24) seemed to show the wake doesn't need autonomous mode either (the dongle woke the PC undriven and disarmed), but the runs below show its EEPROM still had autonomous mode on. Phase A's pass (the wake set) therefore shows the wake works, not that the setting controls it.
- [x] **What the kernel says.** `drivers/media/cec/usb/pulse8/pulse8-cec.c`'s notes: "In autonomous mode, the dongle is known to attempt wakeup if it receives `<User Control Pressed>` ["Power On"], ["Power"] or ["Power Toggle"], or if it receives `<Set Stream Path>` with its own physical address. It also does this if it receives `<Vendor Specific Command>` [0x03 0x00] from an LG TV." The driver turns autonomous mode on whenever a logical address is claimed (`pulse8_cec_adap_log_addr`), and the kernel keeps the claim across a sleep. The C3 sends Set Stream Path to the PC's address on every power-on that lands on the PC's input (step 6b's table).
- [x] **Consequence for step 8.** `on_tv_power_on = "none"` can't stop this adapter waking the PC, and `wake` only matters for an adapter that honors its `power/wakeup`. The remaining levers are below the device: the USB controller's wake (which also stops the keyboard and mouse waking the PC, so not usable), or disabling the adapter's USB port for the sleep (`/sys/bus/usb/devices/usbN/N-0:1.0/usbN-portM/disable`, root-writable, which the root wakeup unit could set before the sleep and clear after the resume). A port test decides whether that works: disable the port, sleep, power the TV on.
- [x] **The port test (2026-10-30).** With the adapter's port disabled (`usb1-port5/disable` = `1`) and the PC asleep, the TV powering on didn't wake the PC, and the keyboard did.
- [x] **Decision: `none` blocks.** `on_tv_power_on` keeps its two values, and `none` now keeps the PC asleep rather than leaving the adapter as the host has it. The wake list names every TV's node with its action (`wake` or `none`; two TVs on one node wake the PC if either asks to). The wakeup unit is a `RemainAfterExit=` oneshot with `StopWhenUnneeded=`, so it starts with `sleep.target` and stops when `sleep.target` stops after the resume. Going to sleep, it arms `wake` adapters as before and disables the USB port of `none` adapters, recording the port before the write; stopping, it enables the ports it disabled. A resume it missed is caught up on the next sleep, and a port or wakeup the host set itself is never undone. Since `none` is the default, every paired USB adapter is now off the bus while the PC sleeps unless its profile asks for the wake.
- [x] **Hardware check, `none` (2026-10-30, 22:47).** The unit cut port 5 at the sleep and restored it at the resume. Turning the TV on didn't wake the PC; after a keyboard wake the adapter re-enumerated and the daemon re-claimed. The input then read "LG DVD Player": the port went down before the driver could disarm (`failed to disable autonomous mode`), and the adapter, still powered, likely answered the TV on its own while the PC slept. The `wake` half moved to the runs below.
- [x] **Why `power/wakeup` doesn't control it (2026-10-30).** Not the host's configuration: the only udev rule touching `power/wakeup` is Steam's (Valve devices only), Solaar's only grants `uaccess` on Logitech `hidraw` nodes, and the keyboards' `enabled` is usbhid's default for boot keyboards. The adapter's firmware raises the USB remote wakeup itself. Pulse-Eight's maintainer said as much on [libcec #546](https://github.com/Pulse-Eight/libcec/issues/546) (2026-07-14): the wake comes from the adapter's firmware, "which would explain … why disabling per-device wake in Windows didn't help". libcec 8.0 (`30c8328f`, 2026-07-13, shipped in 8.1.6) exposes the firmware's autonomous flag as `bAutonomousMode`, saved to the adapter's EEPROM, as the supported way to stop the TV waking the host (`cec-client -am 0`, or `am 0` at its prompt, with `inputattach` stopped).
- [x] **What decides it: the EEPROM flag (2026-10-30/31).** Four timed runs with the port left on (profile `wake`) and CEC Office unmanaged. The EEPROM value is the kernel's read at attach after a USB power cycle (`pulse8_cec` dynamic debug logs `Autonomous mode: on/off`).

  | Run | EEPROM autonomous | Adapter during the sleep | Sleep | Turning the TV on | SIMPLINK power |
  |---|---|---|---|---|---|
  | 23:15 | on (not read; the factory state) | driven, claim cleared | ~2 min | no wake (the TV had forgotten the device after an HDMI replug) | woke |
  | T1, 23:52 | off | driven, claimed | 55 s | no wake | no wake |
  | Control, 00:03 | on | driven, claimed | ~4 min | no wake | woke |
  | 00:14 | off (libcec's write, not read back) | undriven | ~5 min | no wake | no wake |

  The driver's flag doesn't matter: a claim turns autonomous mode on in the adapter's working state and a clear turns it off, yet with the EEPROM off a claimed adapter didn't wake the PC, and with it on a cleared one did. Check B′'s "undriven and disarmed" wake fits: the disarm-on-disconnect patch clears the working state only. Untested: claimed with the EEPROM off over a long sleep (T1 lasted 55 s). Turning the TV on woke the PC in phase A and Check B but in none of these runs, for reasons not pinned down; SIMPLINK's power press was the trigger that told them apart.
- [x] **The port cut is the kernel's own remedy.** `USB_QUIRK_DISCONNECT_SUSPEND` ("Device needs to be disconnected before suspend to prevent spurious wakeup", set with `usbcore.quirks=<vid>:<pid>:m`) calls `usb_port_disable()` at system suspend; upstream sets it for some Huawei LTE modules and a Focusrite Scarlett. The unit does the same per profile through the port's documented `disable` attribute (sysfs-bus-usb, 2022), where the quirk would rule out `wake` for good.
- [x] **A daemon-side lever: `persistent_config=1` (explored 2026-10-31).** With the driver's `persistent_config` parameter set (runtime-writable under `/sys/module/pulse8_cec/parameters/`), its 15 s ping work sends `WRITE_EEPROM` after any claim change (`config_pending`, set by `pulse8_cec_adap_log_addr` on a claim and on a clear), so the EEPROM follows the claim. Read back after USB power cycles: claimed, `Autonomous mode: on` (and the driver restored the stored claim at attach); cleared, `off` (no restore). The write landed 9 to 16 s after each change. With the claim cleared and the write landed, the PC stayed asleep through turning the TV on and SIMPLINK's power press (00:36, 3.5 min, the port left on and `power/wakeup` enabled), against the control run's wake. One daemon dial raced the driver's restore and failed on the kernel's in-progress address (`Invalid value 255 for type LogicalAddress`); the retry connected. Fixed in our linux-cec fork (`03b1b0ef`, [upstream draft](../bugs/upstream-linux-cec-unclaimed-slots.md)).
- [x] **Decision: the port cut ships.** It acts at once from the root unit, needs no host setup, and covers any USB adapter that wakes the PC unasked. The EEPROM lever's write lands up to 15 s after the clear, past logind's 5 s default delay-inhibitor limit, needs the module option on every host, and costs an EEPROM write per claim change. Both it and a driver patch that would make it immediate are recorded in [the idea doc's section 1](../ideas/tv-controls-pc.md#1-the-tv-wakes-the-pc).
- [x] **QA reproduction (2026-10-31, the EEPROM on, `on_tv_power_on = "wake"` so no port cut, CEC Office managed, power-key actions `none`).** Driven by events, not timed:

  | Run | How the PC slept | Sleep | Result |
  |---|---|---|---|
  | Q1, 11:18 | from the desktop, after the TV went off by its remote | 46 s | turning the TV on woke the PC |
  | Q2, 11:30 | followed the TV (`on_tv_power_off = "suspend"`) | 2 min | turning the TV on woke the PC |
  | Q3, 11:38 | followed the TV, off while it showed HDMI 4 | 3.5 min | turning the TV on (on HDMI 4) didn't wake the PC; switching to HDMI 1 did |

  So the TV coming on to the PC's input wakes it, and coming on to another input doesn't, as the driver's notes have it (the adapter wakes on Set Stream Path to its own address). The control run of 00:03, where turning the TV on didn't wake the PC and SIMPLINK's press did, stays unexplained: the input read "LG DVD Player" then and in Q1 to Q3 alike, so the pinned label isn't it.
- [x] **Found during the QA: the TV's CEC side can stick in standby (2026-10-31, 10:28).** At the lock screen, the default user profile's 5 s locked idle timeout turned the screen off and on twice while the password was typed, each emulated over CEC as standby and Image View On. After that the TV answered every power query with standby and ignored Image View On while its panel showed the PC (the LG record read `active`), until it was power-cycled with its remote. Not acted on: recorded for the CEC follow-ups.
- [x] **Two values stay (2026-10-31).** A third that leaves the adapter alone is filed in [the idea doc](../ideas/tv-controls-pc.md#not-yet-planned).
- [x] **`managed` is a kill switch (2026-10-31).** An unmanaged TV is watched only: no profile acts on it, or on this PC for its events. So `managed` now also gates the remote's power key (the daemon's sleep and the session's lock), `remote_keys` (the claim goes without) and the wake list (the unit leaves the adapter as the host has it, which is the "leave alone" state). The input gate (`automate_when`) stays on the ambient trigger only, the TV powering off by itself, since a TV turned off while showing another input says nothing about the PC; the remote's power key is addressed to the PC. Chosen over splitting by direction because a TV paired twice (an unmanaged LG record beside its CEC one) and LG's false power-off reports would otherwise reach the PC.
- [ ] **Still open (2026-10-31):** documenting that `wake` needs the adapter's EEPROM autonomous mode on, which TisVlay can't read, and the one-time `am 0` for users who never want the TV to wake the PC.

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
  - sysfs fixtures: `WAKES_PC` for a USB adapter and not for a DP-AUX one under a GPU, the resolver never panicking, the unit enabling what's listed `wake` and disabling only what it enabled, and blocking `none` adapters' ports for the sleep and enabling them on resume;
  - GUI tests for the new rows; the VM test for the polkit rule.
- [ ] Hardware, on the LG C3, run as unattended blocks since the TV is pox's display:
  - `on_tv_power_off` over CEC (Check A's sequence) and over SSAP, with TisVlay's own power-offs left out;
  - what LG reports during a screen off and on (its `processing` values);
  - a keyboard or Wake-on-LAN wake with the TV in standby, which must not put the PC back to sleep;
  - a system-profile sleep at the login screen, with nobody logged in;
  - `on_remote_power` with passthrough off, the default;
  - toggling `remote_keys` (a re-claim) firing nothing, and Check C with the gate on and the power key dropped;
  - the keymap applied to the live `rc-cec` input (`ir-keytable -r` reading `KEY_RESERVED` for `0x40` and `0x6B` to `0x6D`), and after the re-claim `cec-ctl -d /dev/cec0` showing `(Allow RC Passthrough)` with the OSD name kept; its result fills in the upstream draft's test line;
  - Check B′ with `on_tv_power_on` set to `wake` and to `none`, through the unit; a PC-action sleep with the wake armed staying asleep until the TV comes on; the TV powering on to another input with the wake armed;
  - the TV waking a hibernated PC (the check in progress);
  - an LG turned on with its own remote and then off, which the table above says goes unseen.
- **Results so far (2026-10-25, pox, from the kernel's CEC log and the journals; the scripted runs' first phases slept on the maintainer's default profile, which had `on_tv_power_off = "suspend"`):**
  - over CEC, the TV powering off by itself locked the session and, with the sleep set, suspended the PC; TisVlay's own power-off made no edge on that TV; after a keyboard wake with the TV in standby the PC didn't sleep again (twice);
  - over SSAP, the TV powering off by itself locked the session; a screen off and on made no power-off edge (LG's intermediate reports weren't captured); TisVlay's own power-off made no edge; an LG turned on with its own remote went unseen, as expected;
  - with passthrough off, SIMPLINK's power-off reached the follower, locked the session and suspended the PC: the kernel question is settled;
  - with `remote_keys` on, the re-claim showed `Allow RC Passthrough` with the OSD name kept; the arrows, OK, Exit, CH±, the media keys, Home and Info arrived on the `rc-cec` input; `0x40` arrived as a scan code with no key (the keymap); the follower still got it; turning it off re-claimed without passthrough, name kept;
  - the TV woke the PC through the wakeup unit, which armed the adapter (`1-5`) before the sleep;
  - the TV's wake press: settled by step 6b's rule; check 5's remaining phases: the wake unset (step 8b's port cut), a PC-action sleep with the wake armed (Q2) and another input (Q3), all passed (2026-10-31);
  - open: the GUI's and tray's behavior across a resume (fixed on the train, to recheck).
