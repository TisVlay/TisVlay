# Power and activity vocabulary

TisVlay's names for power and activity grew one feature at a time, so its words cross subjects. "Wake" means the PC resuming, the TV powering on and activity on the lock screen. The PC's sleep is also called "suspend", and its wake also "resume". Proposed (2026-10-25): **one pair of words per subject, each word owned by one subject**, so a word names one concept and a search finds it.

The vocabulary is decided now, so the names the [TV-controls-the-PC plan](../plans/tv-controls-pc.md) introduces follow it from the start. Existing names are renamed after that campaign, as their own breaking change. This doc holds the proposal until research and review settle it; then the glossary moves to a durable home (architecture.md for code, a reference page in the user guide).

## Proposed vocabulary

| Subject | Pair | Also | Notes |
|---|---|---|---|
| The PC | **sleep / wake** | startup, shutdown, reboot | "Suspend" only as logind's method name inside the code. |
| The TV | **power off / power on** | | "Standby" only for a vendor's own state: CEC `<Standby>`, LG's Active Standby, `PowerState::Standby`. Wake-on-LAN keeps its name: it names a mechanism, not the concept. |
| The TV's screen | **screen off / screen on** | | Unchanged. The panel blank, emulated as power where a backend has none. |
| The PC's display | **DPMS off / DPMS on** | | Unchanged: the mechanism's name is already the user-visible one. |
| The session | **lock / unlock** | | Unchanged. |
| Activity | **idle / active** | | For the session, the lock screen and the greeter alike. |

The rule: a word never crosses subjects. A name that needs another subject's word names that subject too, as `on_tv_power_off` does.

## Where words cross today

| Word | What it means today |
|---|---|
| wake | The PC resuming (`on_wake`); the TV powering on (`supports_wake`, `HeldWake`, the wake dial, the GUI's "Wake failed", "Sending wake-up…" and "Wake outcome"); activity on the lock screen (`on_lock_wake`) |
| resume | The PC waking (`SystemEvent::Resume`, the code's name for `on_wake`); activity after idle (`on_idle_resume`, `GreeterResume`) |
| sleep | The PC (`on_sleep`, logind's `PrepareForSleep`) |
| suspend | logind's method for the PC's sleep; an LG TV power state (`PowerState::Suspend`) |
| standby | The TV's off state (CEC `<Standby>`, `PowerState::Standby`, LG's Active Standby) |
| off / on | The TV's actions (`power_off` / `power_on`, `screen_off` / `screen_on`) and the PC's DPMS |

## Renames

After the TV-controls-the-PC campaign, as one breaking change (no aliases):
- **Config keys:** `on_lock_wake` (to something like `on_lock_activity`) and `on_idle_resume` (to something like `on_idle_end`), in whichever profiles carry them. `on_sleep` and `on_wake` already follow the proposal.
- **Code:** `SystemEvent::Resume` to `Wake`, `GreeterResume` to `GreeterActive`, `supports_wake` to `supports_power_on`, `HeldWake` to `HeldPowerOn`, the wake dial to a power-on dial.
- **GUI strings:** "Wake failed", "Sending wake-up…" and "Wake outcome", where they mean the TV's power-on rather than Wake-on-LAN itself.
- **Docs:** the user guide and architecture.md, swept for each word.

The TV-controls-the-PC plan's names, once this settles: `on_tv_standby` and `on_tv_wake` become `on_tv_power_off` and `on_tv_power_on`, the `suspend` action becomes `sleep`, and the host-action types are named to match.

## To research before settling

- **The PC's pair:** the words desktops show (KDE, GNOME, Windows, macOS), and logind's (`Suspend`, `PrepareForSleep`, and whether a newer `Sleep()` method picks the host's configured sleep mode, which would make "sleep" the precise action too).
- **The TV's pair:** the CEC spec's terms, vendor APIs (LG, Samsung, Sony) and Home Assistant's media-player states and services.
- **Activity:** `ext-idle-notify-v1`'s `idled` / `resumed` and Mutter's idle and user-active watches, against "idle / active".
- **The PC's display:** DPMS against Wayland's output-power naming.

## Trigger

Settle before the TV-controls-the-PC plan is implemented, as a fourth area of its review; the renames follow the campaign.
