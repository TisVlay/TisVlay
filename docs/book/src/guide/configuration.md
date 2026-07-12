# Configuration

tisvlay is configured with TOML, spread across a few files:

| File | Owner | Purpose |
|------|-------|---------|
| `/var/lib/tisvlay/daemon.toml` | the daemon (`tisvlay` user) | TV definitions, discovery, and **system** profiles — the daemon's own persisted store |
| `~/.config/tisvlay/session.toml` | each desktop user | **user** profiles and per-user TV profile assignments |
| `~/.config/tisvlay/tray.toml` | each desktop user | tray notification preferences |

`daemon.toml` is the daemon's own store: it reads it at startup and **rewrites it** as state changes, so hand-editing it while the daemon runs will be overwritten. Change TVs and profiles through the CLI (`tisvlay pair`, `tisvlay config-set`, `tisvlay assign`, `tisvlay profile …`, `tisvlay user-profile …`) or the GUI, which persist for you.

To declare TVs **declaratively** — for a reproducible or headless setup — give the daemon a **seed file** with `tisvlay-daemon --seed <file>`. The seed is re-applied on every start, but only for TVs whose `id` is absent from `daemon.toml`, so runtime edits stick and a still-declared TV that you removed at runtime comes back (drop the seed entry to drop it). Omit an entry's `id` and it is derived deterministically from the entry's content.

## TV definitions

Each TV is a `[[tv]]` entry with a `[tv.config]` vendor block:

```toml
version = 1

[[tv]]
id = "9a3f1b22-c4d8-4e90-b21f-7f1a2c8e5b40"   # assigned at pair time (optional in a seed file)
name = "Office"
vendor = "lg"
primary_input = "HDMI_1"   # the input this PC uses (for input-aware automation)
managed = true             # participate in automation

[tv.profile]
system = "00000000-0000-0000-0000-000000005159"   # assigned system profile (by id)

[tv.config]
ip = "192.168.1.170"
mac = "84:90:0a:63:a9:94"
# LG also stores, set automatically during pairing:
#   client_key = "…"   scheme = "wss"
```

A CEC TV uses the same shape with CEC keys instead:

```toml
[[tv]]
name = "Living Room"
vendor = "cec"

[tv.config]
device = "/dev/cec0"            # the kernel CEC node the TV is reachable on
physical_address = "1.0.0.0"    # the adapter's HDMI port (USB adapters only; ignored on GPU DP-CEC)
emulate_screen_via_power = true # honor screen_off/screen_on by powering the TV (CEC can't blank a live panel)
```

In a **seed file** the vendor block can also be written inline: `config = { ip = "192.168.1.170", mac = "84:90:0a:63:a9:94" }`.

CEC has no discrete screen-blank, so with `emulate_screen_via_power` (default `true`) the daemon fulfills the stock `screen_off`/`screen_on` actions by powering the TV off/on; set it `false` to have the TV ignore screen actions instead.

## Discovery

```toml
[discovery]
bind_port = 1901           # SSDP source port (the default; 0 = OS-chosen ephemeral)
poll_interval_ms = 8000    # streaming-discovery loop period
```

The source port can also be set with the daemon flag `tisvlay-daemon --ssdp-port 1901`, or on NixOS with `services.tisvlay.bindPort`. See [Firewall & Discovery](../operations/firewall-discovery.md) for why it matters.

## Profiles

Profiles map **events** to **actions**. There are two kinds:

- **System profiles** (daemon-wide lifecycle): boot, shutdown, reboot, suspend/resume, host-display DPMS blank/wake, and the login-screen (greeter) idle timer.
- **User profiles** (per desktop session): lock, unlock, lock-screen wake, idle timeout/resume, and remote-session connect/disconnect.

Manage them with `tisvlay profile …` (system) and `tisvlay user-profile …` (user), or the GUI's profile editor. A deeper walkthrough of how profiles resolve lives in [Profiles](./profiles.md).

### Action vocabulary

Every event resolves to one of five actions:

| Action | Effect |
|--------|--------|
| `none` | do nothing |
| `screen_off` | OLED-safe blank (TV stays powered) |
| `screen_on` | unblank |
| `power_off` | standby |
| `power_on` | Wake-on-LAN (a CEC bus wake for the HDMI-CEC backend) |

An event value is either the bare action token, or a **table** that adds modifiers:

```toml
on_session_unlock = { action = "screen_on", restore_input = true, while_remote = false }
```

- `restore_input` (default `false`) — after an on-ish action completes, switch the TV to its `primary_input`. Stock-on for the session-start wakes, so a wake lands you back on the PC. Needs a backend that advertises input (LG); inert on CEC. See [Session Automation](./session-automation.md).
- `while_remote` (default `false`) — whether this event's action still fires while the TV is **armed by a remote session**. `false` suppresses it (the suppress-by-default rule); `true` opts the event back into firing while remoted. See [Remote-Session Awareness](./remote-sessions.md).

### System profile

Fields and their stock defaults:

```toml
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_sleep = "power_off"
on_wake = "power_on"
on_dpms_off = "screen_off"       # matched host display blanked (DPMS off)
on_dpms_on = "screen_on"         # matched host display woke (DPMS on)
idle_timeout_seconds = 0         # login-screen (greeter) idle timer; 0 disables it
on_idle_timeout = "screen_off"
on_idle_resume = "screen_on"
automate_when = "always"         # always | on_primary | off_primary
```

### User profile

```toml
on_session_lock = "screen_off"
on_session_unlock = "screen_on"
on_lock_wake = "screen_on"              # you touch the lock screen (before typing)
locked_idle_timeout_seconds = 0         # idle timer while the session is locked; 0 disables
unlocked_idle_timeout_seconds = 0       # idle timer while unlocked; 0 disables
on_idle_timeout = "screen_off"
on_idle_resume = "screen_on"
on_remote_connect = "screen_off"        # a remote session armed the TV
on_remote_disconnect = "screen_off"     # the remote session ended
remote_screencast_arms = false          # let an unattributed screen capture arm the TV
```

The two idle timers set **how long** to wait in each lock state; the shared `on_idle_timeout`/`on_idle_resume` actions decide **what** happens. A `0` timer disables idle for that lock state.

### Inheritance

Any profile field may be omitted; omitted fields inherit through the chain (a TV's assigned profile → the `default` profile → built-in defaults). A profile only needs to spell out what it changes. Setting an event replaces its whole value, so re-gating an inherited action means restating the action too (the GUI does this for you).

### `automate_when`

Gates automation on whether the TV is currently showing the PC. With `on_primary`, automation only fires when the TV is on its `primary_input`; with `off_primary`, only when it is **not** (e.g. don't blank a TV that's showing a game console). `always` ignores the input. This composes with `managed` and with the remote-session gate — see [Profiles](./profiles.md).
