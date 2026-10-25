# Power and activity vocabulary

TisVlay's names for power and activity grew one feature at a time, so its words cross subjects. "Wake" means the PC resuming, the TV powering on and activity on the lock screen. The PC's sleep is also called "suspend", and its wake also "resume". "Remote" means both a remote session and the TV's remote control. Decided (2026-10-25, after the research below): **one pair of words per subject, each word owned by one subject**, so a word names one concept and a search finds it.

The vocabulary applies now: the names the [TV-controls-the-PC plan](../plans/tv-controls-pc.md) introduces follow it from the start. Two renames land inside that campaign, as its prerequisites: remote sessions become [streams](#streams), before the plan's remote-control steps take the word "remote", and the GUI's power-state labels get keyed on the wire's tokens. The other existing names are renamed after the campaign, as one breaking change. This doc holds the glossary until then; it then moves to a durable home (architecture.md for code, a reference page in the user guide).

## Vocabulary

| Subject | Verbs | States | Notes |
|---|---|---|---|
| The PC | **sleep / wake**; also startup, shutdown, reboot | asleep / awake | "Sleep" is the generic word. Specific sleep modes keep logind's names (suspend, hibernate), as values where the mode matters, such as the system profile's `suspend` and `sleep` actions. "Resume" never names the PC, though it's the kernel's word. In identifiers the PC is **`pc`** (`WAKES_PC`, "PC actions"), never "host", "machine" or "desktop". |
| The TV | **power off / power on** | **standby / on** | Events and actions use the verbs (`power_off`, `on_tv_power_off`). "Standby" names the state, which is every vendor's name for it (`PowerState::Standby`, CEC power status, Sony, Samsung, LG's Active Standby, the GUI's status text). Wake-on-LAN keeps its name: it names a mechanism, not the concept. |
| The TV's remote control | **the remote** (its keys, its power key) | | Free once streams have their own word: `on_remote_power`, `REMOTE_KEYS`. |
| The TV's screen | **screen off / screen on** | | Unchanged. The panel blank, emulated as power where a backend has none. |
| The PC's display | **DPMS off / DPMS on** | | Unchanged: the mechanism's name is already the user-visible one. DPMS's own Standby and Suspend modes stay inside the mechanism; TisVlay uses only off and on. |
| The session | **lock / unlock** | locked / unlocked | Unchanged. |
| Activity | **idle / resume** | idle / active | "Resume" means only activity after idle, always qualified (`on_idle_resume`, greeter resume), as in `ext-idle-notify-v1`, swayidle, hypridle and KIdleTime. |
| Desktop streaming | **a stream starts / ends** | a stream arms a TV | Today's "remote session": Sunshine and Moonshine, screen capture, remote desktop. "Connect" stays the TV link's word. See [Streams](#streams). |

The rule: a word never crosses subjects. A name that needs another subject's word names that subject too, as `on_tv_power_off` does.

Platform and vendor names keep their spelling wherever they name the platform's own thing: logind's methods and `PrepareForSleep`, polkit's `login1.suspend`, the kernel's `power/wakeup` and "PM: suspend entry", `ext-idle-notify`'s `resumed`, PipeWire's `Suspended` / `Idle` node states, linux-cec's `AsyncDevice::wake`, CEC's messages, LG's and other vendors' power states, DPMS's modes.

## Gaps the review found (2026-10-25)

Decided where marked, proposals otherwise:
- **The TV's "on" is two tests (decided).** `PowerState::is_on` (on the wire as `state/power.is_on`) counts LG's Active Standby as on, since its network card is alive, while `is_awake`, which the GUI, CLI and tray show, doesn't. Both retire for the TV-controls-the-PC plan's three classes (on, off, transient), which become the one classification: each caller picks by class, and the wire carries the class instead of `is_on`. Active Standby is off.
- **The two event directions (decided): going-off / coming-on.** They replace "off-going" and "wake-ish" in code, architecture.md and the user guide: going-off events (sleep, shutdown, the blanks, the TV powering off) fire under a stream, coming-on events (startup, wake, DPMS on, the greeter's resume) are held back.
- **"Blank", "dark" and "lit"** cover both the TV's screen and the PC's display. Proposal: screen off / on for the TV's screen, DPMS off / on for the display, and neither "blank" nor "dark" in identifiers ("dark intents" become off intents).
- **"Power events"** means the PC's lifecycle in the CLI's `profile` help and the world's events. Proposal: PC events; "power" stays the TV's verb.
- **"Active"** also names logind's active session and user, `PowerState::Active`, CEC's `<Active Source>` and the active input. Those keep their platform meaning; activity's own state word is only ever "active" next to "idle".
- **An unreachable TV** prints as `off` in the CLI, though TisVlay can't observe a power state it can't reach. Open: whether that's a link state (unreachable) rather than a power state.
- **"Boot"** is the PC's startup in the GUI ("boot / shutdown / sleep") and a TV's own boot in the troubleshooting chapter. Proposal: startup for the PC.
- **"Greeter"** means both the lock screen's prompt (`on_lock_wake`'s doc) and the login screen. Proposal: greeter for the login screen only, lock screen for the other.

## Where words cross today

| Word | What it means today |
|---|---|
| wake | The PC waking (`on_wake`); the TV powering on (`supports_wake`, `HeldWake`, the wake dial, the GUI's "Wake failed", "Sending wake-up…" and "Wake outcome", the tray's "Waking…" toasts); activity on the lock screen (`on_lock_wake`); the direction of an event ("wake-ish") |
| awake | The TV (`is_awake`, the CLI's "connected and awake", the pairing wizard's "only answer while awake"); the PC in prose |
| resume | The PC waking (`SystemEvent::Resume`, the logind watcher's "system resumed"); activity after idle (`on_idle_resume`, `GreeterResume`, the unqualified `event/session/resume`); the TV's screen ("screen-blank with instant resume") |
| sleep | The PC (`on_sleep`, logind's `PrepareForSleep`) |
| suspend | logind's method for the PC's sleep; an LG TV power state (`PowerState::Suspend`); the PC's sleep in general, in the user guide |
| standby | The TV's off state (CEC `<Standby>`, `PowerState::Standby`, LG's Active Standby), but `is_on` counts Active Standby as on |
| power cycle | A power-on: the GUI's `power_cycle` module and `power-cycle-phase`, the SDK's `power_cycle_progress_stream`, the tray's power-cycle toasts |
| remote | Remote sessions (`on_remote_connect`, `while_remote`, `remote/session`) and, in the plan, the TV's remote control |
| blank / dark | The TV's screen (`ScreenOff`'s "OLED-safe blank") and the PC's display (`on_dpms_off`'s "connector blanks") |
| off / on | The TV's actions (`power_off` / `power_on`, `screen_off` / `screen_on`), the PC's DPMS, and an unreachable TV in the CLI |
| Wake-on-LAN | The mechanism, but also every power-on: `progress/power`'s `wol_sent` goes out for CEC's Image View On too, and the GUI's "Wake outcome" setting describes Wake-on-LAN attempts while also firing for CEC |

## Streams

Done (2026-10-25), before the plan's remote-control steps, as one breaking change (no aliases):
- **Config keys:** `on_remote_connect` / `on_remote_disconnect` became `on_stream_start` / `on_stream_end`, the `while_remote` modifier `while_streaming`, and `remote_screencast_arms` `screencast_arms` (whether a bare screencast arms a stream). `on_local_return` stays: it names the local person's return.
- **Wire:** `remote/session` became `stream/state`, `user/remote/session` `user/stream/state`, and `user/remote/rpc/<verb>` `user/stream/rpc/<verb>`. The TV meta's `remote_armed` became `stream_armed`.
- **Types:** `RemoteSessionState`, `RemoteSource`, `RemoteSourceKind`, `RemoteSet`, `RemoteReport`, `RemoteClient` and `RemoteArmed` became `StreamState`, `StreamSource`, `StreamSourceKind`, `StreamSet`, `StreamReport`, `StreamClient` and `StreamArmed`. The SDK's `SessionClient::remote_session` became `stream_state`, and `TvInfo::remote_armed` `stream_armed`.
- **CLI and GUI:** the `remote-session` verb became `stream`, the editor's card "STREAMING", its toggle "Fires while streaming", and the tray's line "Stream active".
- **Docs:** docs/remote/ became docs/streams/, and the user guide's remote-sessions chapter `streaming.md` ("Streaming").
- **Kept:** logind's own remote sessions (`Remote=yes`, `RemoteHost`) and the portal's `RemoteDesktop` interface keep their platform names, the TV's remote control keeps "remote", and test fixtures keep their labels.

## Renames after the campaign

One breaking change (no aliases), grouped by what each one breaks:
- **Config keys:**
  - `on_lock_wake` (to something like `on_lock_resume`);
  - the tray's `tv_wake` setting;
  - the mock's `wake` field;
  - the doc comments that docgen copies into the user guide ("system sleep/suspend", "wake/resume", "connector blanks").
  - LG's `wake_id` MAC alias is Wake-on-LAN's own and may stay. `on_sleep`, `on_wake` and `on_idle_resume` already follow the vocabulary.
- **Wire:**
  - `state/link.supports_wake`;
  - `state/power.is_on`, replaced by the power class;
  - `event/session/lock_wake`, and `event/session/resume` qualified as an idle resume;
  - the `wake-not-supported` error code and its message;
  - `progress/power`'s `wol_sent` for power-ons that aren't Wake-on-LAN;
  - `user/tray/settings.tv_wake`.
- **Public API of the published crates:**
  - tisvlay-core: `TvConnector::supports_wake`, `wake` and `wake_and_connect`; `PowerState::is_awake` and `is_on`, retiring for the power classes, and `needs_wake_preflight`; and the doc comments calling `PowerOn` and `Capabilities::POWER` Wake-on-LAN.
  - tisvlay-protocol: `ErrorCode::WakeNotSupported`, `NotificationSettings::tv_wake` and `PowerPhase::WolSent`.
  - The SDK: `TvInfo::supports_wake`, `NotificationCategory::TvWake` and `power_cycle_progress_stream`.
  - Whether `SessionAction`, which acts on the TV, keeps its name is open.
- **Code:**
  - `SystemEvent::Resume` to `Wake`;
  - the `HeldWake` family to held power-ons;
  - the wake dial (`PendingDial::wake`, `wake_dial`, `wake_needy`) to a power-on dial;
  - the session's lock-wake family (`LockWake`, `lock_wake_policy`, `LOCK_WAKE_TAG`, `gated_wake`);
  - LG's `WAKE_CONNECT_*` and CEC's `WAKE_VERIFY_*`;
  - the GUI's `power_cycle` module and `power-cycle-phase`, and the tray's `TvWake`;
  - log messages ("system resumed", "online after wake", "a held wake expired", "sending lock wake").
- **Strings:**
  - the tray's "Power On (Wake)" and its "Waking…" / "is awake" toasts;
  - the GUI's "Wake outcome", "Sending wake-up…", "Wake failed", "wake from standby with WoL", "screen-blank with instant resume", "only answer while awake", "power-cycle this TV" and "On lock wake";
  - the CLI's help ("connected and awake", "Blank or wake a TV's screen", "power events").
- **Docs:**
  - the user guide (about 95 hits in 16 files, worst in automation.md, quick-start.md, remote-sessions.md and gui-tray.md);
  - architecture.md;
  - docs/watchers, docs/cec, docs/backends, docs/remote and docs/testing.md;
  - two file names (lg/benchmarks/wake-reconnect.md, concerns/cec-standby-ignored-after-wake.md).
  - docs/design.md is retired and stays as it is.

## Research (2026-10-25)

- **The PC.**
  - Plasma 6.8's catalogs on pox label it "Sleep" (described as "Suspend to RAM") and "Hibernate". PowerDevil renamed its internal power-button actions to Sleep and Hibernate because "Sleep" can mean several modes depending on configuration ([powerdevil!276](https://invent.kde.org/plasma/powerdevil/-/merge_requests/276)).
  - Windows and macOS say "Sleep". GNOME says "Suspend", per older sources; its current label wasn't confirmed.
  - logind (systemd 262 on pox) has `Suspend()`, `Hibernate()`, `HybridSleep()`, `SuspendThenHibernate()` and, since 256, `Sleep()`. `Sleep()` picks the first supported mode from `SleepOperation=` (default "suspend-then-hibernate suspend hibernate"). On pox that's suspend-then-hibernate, which hibernates after `HibernateDelaySec=`'s 2 h default on a machine without a battery. Its signal is `PrepareForSleep`.
- **The TV.** Every source calls the off state "standby" and the commands "power" or "turn" on/off:
  - CEC power status: On, Standby, and the transitions between them; the `<Standby>` and `<Image View On>` messages.
  - Sony: `active` / `standby`, set with `setPowerStatus` ([Sony BRAVIA REST API](https://pro-bravia.sony.net/develop/integrate/rest-api/spec/service/system/v1_0/getPowerStatus/)).
  - Samsung's consumer API: `PowerState` `on` / `standby` (community reports, [Samsung developer forum](https://forum.developer.samsung.com/t/check-tv-status-ambient-mode-or-normal-mode/9699)).
  - LG: Active, Active Standby, Suspend, Screen Off.
  - Home Assistant's media player: `on`, `off`, `standby` and `idle` states, `turn_on` / `turn_off` actions ([media player entity](https://developers.home-assistant.io/docs/core/entity/media-player)).
- **Activity.**
  - `ext-idle-notify-v1`: `idled` / `resumed` (KWin, wlroots and others; not Mutter).
  - swayidle: `timeout` / `resume`. hypridle: `on-timeout` / `on-resume`. KDE's KIdleTime: `timeoutReached` / `resumingFromIdle`.
  - Mutter: idle watches and user-active watches. logind: `IdleHint`.
- **The PC's display.** VESA DPMS has On, Standby, Suspend and Off modes, all four in KDE's DPMS Wayland protocol; the kernel's sysfs attribute reads On or Off.
- **Streams.** Sunshine and Moonlight call a session a stream, and PipeWire calls its captures streams; remote-desktop users say remote desktop. "Cast" would collide with the Chromecast on the TV's other input, and "capture" with the InputCapture portal the detectors deliberately exclude.

## Trigger

The streams rename and the GUI's power-state labels are prerequisites of the TV-controls-the-PC plan. The glossary moves to its durable home at that plan's merge; the other renames follow the campaign.
