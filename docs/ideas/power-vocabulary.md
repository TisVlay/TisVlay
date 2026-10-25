# Power and activity vocabulary

TisVlay's names for power and activity grew one feature at a time, so its words cross subjects. "Wake" means the PC resuming, the TV powering on and activity on the lock screen. The PC's sleep is also called "suspend", and its wake also "resume". Decided (2026-10-25, after the research below): **one pair of words per subject, each word owned by one subject**, so a word names one concept and a search finds it.

The vocabulary applies now: the names the [TV-controls-the-PC plan](../plans/tv-controls-pc.md) introduces follow it from the start. Existing names are renamed after that campaign, as their own breaking change. This doc holds the glossary until the plan's review has been through it as a fourth area; then the glossary moves to a durable home (architecture.md for code, a reference page in the user guide).

## Vocabulary

| Subject | Verbs | States | Notes |
|---|---|---|---|
| The PC | **sleep / wake**; also startup, shutdown, reboot | asleep / awake | "Sleep" is the generic word. Specific sleep modes keep logind's names (suspend, hibernate), as values where the mode matters, such as the system profile's `suspend` and `sleep` actions. "Resume" never names the PC, though it's the kernel's word. |
| The TV | **power off / power on** | **standby / on** | Events and actions use the verbs (`power_off`, `on_tv_power_off`). "Standby" names the state, which is every vendor's name for it (`PowerState::Standby`, CEC power status, Sony, Samsung, LG's Active Standby, the GUI's status text). Wake-on-LAN keeps its name: it names a mechanism, not the concept. |
| The TV's screen | **screen off / screen on** | | Unchanged. The panel blank, emulated as power where a backend has none. |
| The PC's display | **DPMS off / DPMS on** | | Unchanged: the mechanism's name is already the user-visible one. DPMS's own Standby and Suspend modes stay inside the mechanism; TisVlay uses only off and on. |
| The session | **lock / unlock** | locked / unlocked | Unchanged. |
| Activity | **idle / resume** | idle / active | "Resume" means only activity after idle, always qualified (`on_idle_resume`, greeter resume), as in `ext-idle-notify-v1`, swayidle, hypridle and KIdleTime. |

The rule: a word never crosses subjects. A name that needs another subject's word names that subject too, as `on_tv_power_off` does.

## Where words cross today

| Word | What it means today |
|---|---|
| wake | The PC waking (`on_wake`); the TV powering on (`supports_wake`, `HeldWake`, the wake dial, the GUI's "Wake failed", "Sending wake-up…" and "Wake outcome"); activity on the lock screen (`on_lock_wake`) |
| resume | The PC waking (`SystemEvent::Resume`, the code's name for `on_wake`); activity after idle (`on_idle_resume`, `GreeterResume`) |
| sleep | The PC (`on_sleep`, logind's `PrepareForSleep`) |
| suspend | logind's method for the PC's sleep; an LG TV power state (`PowerState::Suspend`) |
| standby | The TV's off state (CEC `<Standby>`, `PowerState::Standby`, LG's Active Standby) |
| off / on | The TV's actions (`power_off` / `power_on`, `screen_off` / `screen_on`) and the PC's DPMS |

## Renames

After the TV-controls-the-PC campaign, as one breaking change (no aliases):
- **Config keys:** `on_lock_wake`, activity on the lock screen, to something like `on_lock_resume`. `on_sleep`, `on_wake` and `on_idle_resume` already follow the vocabulary.
- **Code:** `SystemEvent::Resume` to `Wake`, `supports_wake` to `supports_power_on`, `HeldWake` to `HeldPowerOn`, the wake dial to a power-on dial. `GreeterResume` stays.
- **GUI strings:** "Wake failed", "Sending wake-up…" and "Wake outcome", where they mean the TV's power-on rather than Wake-on-LAN itself.
- **Docs:** the user guide and architecture.md, swept for each word.

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

## Trigger

The glossary moves to its durable home after the TV-controls-the-PC plan's review; the renames follow the campaign.
