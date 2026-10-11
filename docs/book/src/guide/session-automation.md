# Session Automation

Two services drive automation, each through its own profile kind:

- **`tisvlay-session`** (a per-user service) watches *your desktop session* — lock, unlock, lock-screen interaction, idle, and remote sessions — and dispatches the **user profile** actions.
- **`tisvlay-daemon`** (the system daemon) watches *machine lifecycle* — boot, shutdown, suspend/resume, host-display power — and dispatches the **system profile** actions.

Each event's field, and its stock action, is in [Configuration](./configuration.md#profiles); which TVs react is governed by the [automation gate](./profiles.md#the-automation-gate).

## Session events (user profile)

| Event | Field |
|-------|-------|
| Session locks | `on_session_lock` |
| You interact with the lock screen | `on_lock_wake` |
| Session unlocks | `on_session_unlock` |
| Idle timer elapses (per lock state) | `on_idle_timeout` |
| Activity resumes after idle | `on_idle_resume` |
| A remote session arms the TV | `on_remote_connect` |
| The remote session ends | `on_remote_disconnect` |
| First local input after a remote session ended | `on_local_return` |

Notes:

- **Lock wake** exists so the TV is lit *before* you type your password: the moment you wiggle the mouse at the lock screen, `on_lock_wake` fires — the later unlock is then a no-op. (Wayland sessions only.)
- **Idle** has two timers — `locked_idle_timeout_seconds` and `unlocked_idle_timeout_seconds` — each firing only in its own lock state, so you can blank aggressively at the lock screen without touching the active-session timer. Both are off until you set them, and idle is independent of your desktop's own screen-locking/DPMS timers. If a stray remote press lights the panel while an idle blank is meant to hold, the daemon re-blanks after one more interval — closing a gap the edge-triggered desktop timers can't see.
- **Remote sessions** arm the TV, suppress the wake events above while armed, and relight the panel on your first local input afterward. See [Remote-Session Awareness](./remote-sessions.md).
- **Multiple users**: only the seat's *foreground* session dispatches this automation. With fast user switching, a backgrounded user's lock/idle events are suppressed until they're the active session again — so on a shared TV, a switched-away account can't blank or wake the panel out from under whoever is actually using the machine.
- **Input restore**: the session-start wakes carry `restore_input`, so a wake also switches the TV back to your PC's input (on backends that can switch inputs) — you return to the desktop, not to whatever input was last selected. Set the TV's `primary_input` for this to take effect (see [Configuration](./configuration.md#tv-definitions)).

A typical day with the defaults: lock the screen → TV blanks (OLED-safe, instant resume) → come back and tap a key → TV is already lit when the password prompt appears → unlock → nothing left to do but land on the PC input. Suspend the machine → `on_sleep` powers the TV off → resume → `on_wake` wakes it.

## System events (daemon)

Boot (`on_startup`), shutdown (`on_shutdown`), reboot (`on_reboot`), suspend (`on_sleep`), and resume (`on_wake`) are handled by the daemon — no desktop session required.

The daemon also mirrors a **host display's power state** onto the TV that shares its connector: `on_dpms_off` / `on_dpms_on` fire when your desktop blanks or wakes the display the TV is attached to. The stock actions blank and unblank the TV, so this works out of the box — and it covers the window before you log in, when no user session is running yet. When a session *is* running, the lock/idle path above and this DPMS path can both fire for the same blank; that overlap is harmless by design (`screen_off` is idempotent, and a duplicate `screen_on` is suppressed while the TV is mid-reconnect).

### Login-screen idle (greeter)

The DPMS mirror only reacts to a blank the compositor *initiates*; an unattended **login screen** never blanks on its own (the desktop's idle timer doesn't run before login). The optional **`tisvlay-greeter`** client closes that gap: it watches the greeter compositor's idle and drives the **system** profile's `idle_timeout_seconds` / `on_idle_timeout` / `on_idle_resume` — the same idle shape as a logged-in session, but at the login screen. Blanking stays off until you set a non-zero `idle_timeout_seconds` on a system profile. Two greeters are supported: **GDM** (GNOME's login screen — the distro packages ship this variant enabled, and it needs no manual setup) and **plasma-login-manager** (KDE's); on NixOS the client turns on by itself when either is the display manager (see [Installation](../getting-started/installation.md#nixos-flake-module)). It also lights the panel once at startup, so the login screen is never stranded dark after a logout.

## Checking that it's running

Session events need the user service alive:

```sh
systemctl --user status tisvlay-session
journalctl --user -u tisvlay-session -f
```

If an event fires but the TV does nothing, see [Automation does nothing](../operations/troubleshooting.md#automation-does-nothing).

## Reliability

Actions are verified and retried, survive reconnects, and a boot- or resume-time wake waits for the network — so automation works from any starting state without you scripting the difference. See [Networking & Reliability](../operations/networking-reliability.md).
