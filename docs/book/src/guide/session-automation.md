# Session Automation

Two services drive automation, each through its own profile kind:

- **`tisvlay-session`** (a per-user service) watches *your desktop session* — lock, unlock, lock-screen interaction, idle — and dispatches the **user profile** actions.
- **`tisvlayd`** (the system daemon) watches *machine lifecycle* — boot, shutdown, suspend/resume, host-display power — and dispatches the **system profile** actions.

Field-by-field configuration is in [Configuration](./configuration.md#profiles); which TVs react is governed by the [automation gate](./profiles.md#the-automation-gate).

## Session events (user profile)

| Event | Field | Default |
|-------|-------|---------|
| Session locks | `on_session_lock` | `screen_off` |
| You interact with the lock screen | `on_lock_wake` | `screen_on` |
| Session unlocks | `on_session_unlock` | `screen_on` |
| No input for `idle_timeout_seconds` | `on_idle_timeout` | `screen_off` |
| Activity resumes after idle | `on_idle_resume` | `screen_on` |

Notes:

- **Lock wake** exists so the TV is lit *before* you type your password: the moment you wiggle the mouse at the lock screen, `on_lock_wake` fires — the later unlock is then a no-op. (Wayland sessions only.)
- **Idle** is off by default (`idle_timeout_seconds = 0`). It is independent of your desktop's own screen-locking/DPMS timers — it blanks the TV even if the desktop never locks.

A typical day with the defaults: lock the screen → TV blanks (OLED-safe, instant resume) → come back and tap a key → TV is already lit when the password prompt appears → unlock → nothing left to do. Suspend the machine → `on_sleep` powers the TV off → resume → `on_wake` wakes it via Wake-on-LAN.

## System events (daemon)

Boot (`on_startup`), shutdown (`on_shutdown`), reboot (`on_reboot`), suspend (`on_sleep`), and resume (`on_wake`) are handled by the daemon — no desktop session required.

The daemon also mirrors a **host display's power state** onto the TV that shares its connector: `on_dpms_off` / `on_dpms_on` fire when your desktop blanks or wakes the display the TV is attached to. They default to `screen_off` / `screen_on`, so this works out of the box — and it covers the window before you log in, when no user session is running yet. When a session *is* running, the lock/idle path above and this DPMS path can both fire for the same blank; that overlap is harmless by design (`screen_off` is idempotent, and a duplicate `screen_on` is suppressed while the TV is mid-reconnect). A useful property: `on_dpms_on = "screen_on"` escalates to a full power-on for a TV in standby, so "wake the TV with my display" works from any state.

### Login-screen idle (greeter)

The DPMS mirror only reacts to a blank the compositor *initiates*; an unattended **login screen** never blanks on its own (the desktop's idle timer doesn't run before login). The optional **`tisvlay-greeter`** client closes that gap: it watches the greeter compositor's idle and drives the **system** profile's `idle_timeout_seconds` / `on_idle_timeout` / `on_idle_resume` (default `0` / `screen_off` / `screen_on`) — the same idle shape as a logged-in session, but at the login screen. Turn it on by setting `idle_timeout_seconds` on a system profile and installing the client (`services.tisvlay.greeter.enable` on NixOS); it is off by default. It also lights the panel once at startup, so the login screen is never stranded dark after a logout.

## Checking that it's running

Session events need the user service alive:

```sh
systemctl --user status tisvlay-session
journalctl --user -u tisvlay-session -f
```

If an event fires but the TV does nothing, see [Automation does nothing](../operations/troubleshooting.md#automation-does-nothing).

## Reliability

Screen actions are verify-and-retry, not fire-and-forget: the daemon confirms the panel actually changed state, retries if it didn't, and escalates `screen_on` to a Wake-on-LAN power-on when the TV turns out to be in standby. The *desired* screen state is also latched, so it survives a TV reconnect or the login-screen→session handoff rather than being lost. Automation works from any starting state without you scripting the difference.
