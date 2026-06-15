# Configuration

tisvlay is configured with TOML. There are three config files:

| File | Owner | Purpose |
|------|-------|---------|
| `/var/lib/tisvlay/config.toml` | the daemon (`tisvlay` user) | TV definitions, discovery, and **system** profiles |
| `~/.config/tisvlay/session.toml` | each desktop user | **user** profiles and per-user TV profile assignments |
| `~/.config/tisvlay/tray.toml` | each desktop user | tray notification preferences |

Most edits are easier through the CLI (`tisvlay pair`, `tisvlay manage`, `tisvlay profile …`) or the GUI, but the schema is straightforward to edit by hand. After a manual edit, reload without restarting — each service has a `ReloadConfig`:

```sh
# Daemon config (system bus; drop sudo and add --user for a dev-mode daemon)
sudo busctl call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig
# User config (session bus)
busctl --user call org.tisvlay.Session /org/tisvlay/Session org.tisvlay.Session1 ReloadConfig
```

## TV definitions

Each TV is a `[[tv]]` entry plus a vendor table:

```toml
[[tv]]
uuid = "9a3f1b22-c4d8-4e90-b21f-7f1a2c8e5b40"
name = "office"
vendor = "lg"
primary_input = "HDMI_1"   # input this TV uses for the PC (for input-aware automation)
managed = true             # participate in automation

[tv.profile]
system = "00000000-0000-0000-0000-000000005159"   # assigned system profile (by UUID)

[tv.lg]
ip = "192.168.1.170"
mac = "84:90:0a:63:a9:94"
client_key = "…"           # set during pairing
scheme = "wss"
```

## Discovery

```toml
[discovery]
bind_port = 1901           # SSDP source port (the default; 0 = OS-chosen ephemeral)
poll_interval_ms = 8000    # streaming-discovery loop period
```

See [Firewall & Discovery](../operations/firewall-discovery.md) for why `bind_port` matters.

## Profiles

Profiles map **events** to **actions**. There are two kinds:

- **System profiles** (daemon-wide lifecycle): boot, shutdown, reboot, suspend/resume, host-display DPMS blank/wake, and the login-screen (greeter) idle timer.
- **User profiles** (per desktop session): lock, unlock, lock-screen wake, idle timeout/resume.

### Action vocabulary

Every event field takes one of:

| Action | Effect |
|--------|--------|
| `none` | do nothing |
| `screen_off` | OLED-safe blank (TV stays powered) |
| `screen_on` | unblank |
| `power_off` | standby |
| `power_on` | Wake-on-LAN |

### System profile

```toml
[system.profiles.00000000-0000-0000-0000-000000005159]
name = "default"
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_sleep = "power_off"
on_wake = "power_on"
on_dpms_off = "screen_off"  # matched host display blanked (DPMS off)
on_dpms_on = "screen_on"    # matched host display woke (DPMS on)
idle_timeout_seconds = 0    # login-screen (greeter) idle timer; 0 disables it
on_idle_timeout = "screen_off"
on_idle_resume = "screen_on"
automate_when = "always"    # always | on_primary | off_primary
```

### User profile

```toml
[user.profiles.00000000-0000-0000-0000-00000000515a]
name = "default"
on_session_lock = "screen_off"
on_session_unlock = "screen_on"
on_lock_wake = "screen_on"        # interaction with the lock screen (Wayland)
idle_timeout_seconds = 0          # 0 disables the idle timer
on_idle_timeout = "screen_off"
on_idle_resume = "screen_on"
```

### Inheritance

Any profile field may be omitted; omitted fields inherit from the profile chain (a TV's assigned profile → the `default` profile → built-in defaults). This means a profile only needs to spell out what it changes.

### `automate_when`

Gates automation on whether the TV is currently showing the PC. With `on_primary`, automation only fires when the TV is on its `primary_input`; with `off_primary`, only when it is **not** (e.g. don't blank a TV that's showing a game console). `always` ignores the input.

A deeper walkthrough of profile resolution lives in [Profiles](./profiles.md).
