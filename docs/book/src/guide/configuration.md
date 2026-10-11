# Configuration

tisvlay is configured with TOML, spread across a few files:

| File | Owner | Purpose |
|------|-------|---------|
| `/var/lib/tisvlay/daemon.toml` | the daemon (`tisvlay` user) | TV definitions, discovery, and **system** profiles — the daemon's own persisted store |
| `~/.config/tisvlay/session.toml` | each desktop user | **user** profiles and per-user TV profile assignments |
| `~/.config/tisvlay/tray.toml` | each desktop user | tray notification preferences |

`daemon.toml` is the daemon's own store: it reads it at startup and **rewrites it** as state changes, so hand-editing it while the daemon runs will be overwritten. Change TVs and profiles through the CLI (`tisvlay pair`, `tisvlay config-set`, `tisvlay assign`, `tisvlay profile …`, `tisvlay user-profile …`) or the GUI, which persist for you.

To declare TVs **declaratively** — for a reproducible or headless setup — give the daemon a **seed file** with `tisvlay-daemon --seed <file>` (on NixOS, the module's `settings` option writes it for you). The seed is re-applied on every start, but only for TVs whose `id` is absent from `daemon.toml`, so runtime edits stick and a still-declared TV that you removed at runtime comes back (drop the seed entry to drop it). Omit an entry's `id` and it is derived deterministically from the entry's content.

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
system = "…"               # assigned system profile, by id (see `tisvlay profile list`)

[tv.config]
ip = "192.168.1.170"
mac = "84:90:0a:63:a9:94"  # Wake-on-LAN; read from the TV at pair time if left out
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
advertise_osd_name = true       # answer the TV's name poll, so it labels this PC's HDMI input
osd_name = "TisVlay"            # the label the TV shows for the input (14 characters max)
```

In a **seed file** the vendor block can also be written inline: `config = { ip = "192.168.1.170", mac = "84:90:0a:63:a9:94" }`.

With `emulate_screen_via_power` (default `true`), the daemon fulfills `screen_off`/`screen_on` on a CEC TV by powering it off/on, since CEC can't blank a panel that stays on; set it `false` to have the TV ignore screen actions instead.

With `advertise_osd_name` (default `true`), tisvlay answers the TV's name poll on the CEC bus, so the TV labels the PC's HDMI input `osd_name` in its input picker — the way a Chromecast names its own input. Set it `false` to stay silent and keep the TV's generic input label.

## Discovery

```toml
[discovery]
bind_port = 1901           # SSDP source port (0 = OS-chosen ephemeral)
poll_interval_ms = 8000    # streaming-discovery loop period
```

[Firewall & Discovery](../operations/firewall-discovery.md#open-the-pinned-source-port) explains the source port and its other ways to set it.

## Profiles

Profiles map **events** to **actions**. There are two kinds:

- **System profiles** (daemon-wide lifecycle): boot, shutdown, reboot, suspend/resume, host-display DPMS blank/wake, and the login-screen (greeter) idle timer.
- **User profiles** (per desktop session): lock, unlock, lock-screen wake, idle timeout/resume, remote-session connect/disconnect, and the local return after a remote session.

Manage them with `tisvlay profile …` (system) and `tisvlay user-profile …` (user), or the GUI's profile editor. This page is the schema; how profiles inherit from each other and which TVs they act on is in [Profiles](./profiles.md).

### Action vocabulary

Every event resolves to one of these actions:

| Action | Effect |
|--------|--------|
| `none` | do nothing |
| `screen_off` | OLED-safe blank (TV stays powered) |
| `screen_on` | unblank |
| `power_off` | standby |
| `power_on` | wake from standby (Wake-on-LAN for network TVs, a bus wake for HDMI-CEC) |

An event value is either the bare action token, or a **table** that adds modifiers:

```toml
on_session_unlock = { action = "screen_on", restore_input = true, while_remote = false }
```

- `restore_input` (default `false`) — after an on-ish action completes, switch the TV to its `primary_input`. Needs a backend that can switch inputs (LG; not CEC).
- `while_remote` (default `false`) — `true` lets the action fire even while a remote session has the TV armed; see [Remote-Session Awareness](./remote-sessions.md#what-it-does-when-armed).

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
automate_when = "always"         # always | on_primary | off_primary (see Profiles → The automation gate)
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
on_local_return = { action = "screen_on", restore_input = true }  # first local input after a remote disconnect
remote_screencast_arms = false          # let an unattributed screen capture arm the TV
```

The two idle timers set **how long** to wait in each lock state; the shared `on_idle_timeout`/`on_idle_resume` actions decide **what** happens. A `0` timer disables idle for that lock state.

Any field may be omitted from a profile; it then inherits (see [Profiles → Resolution order](./profiles.md#resolution-order)). Setting an event replaces its whole value, so re-gating an inherited action means restating the action too (the GUI does this for you).
