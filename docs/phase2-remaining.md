# Future Work

## Phase 2 Status: COMPLETE

All core Phase 2 functionality is implemented in `crates/orlgtvd/`. See source code for details:
- [dbus.rs](../crates/orlgtvd/src/dbus.rs) - D-Bus interface (`org.orlgtv.Daemon1`)
- [tv_manager.rs](../crates/orlgtvd/src/tv_manager.rs) - Connection management, reconnection, state events
- [main.rs](../crates/orlgtvd/src/main.rs) - Signal handling, D-Bus setup

---

## Optional Enhancements

### CLI D-Bus Migration

**Current**: CLI connects directly to TVs via WebSocket.
**Goal**: CLI uses daemon via D-Bus, with `--direct` fallback.

Benefits: Centralized connection management, automatic reconnection, config handled by daemon.

Implementation: Add `zbus` dependency, create D-Bus client module, add `--direct` flag.

### SSDP Discovery

**Goal**: Automatically discover LG TVs on the network.

Implementation: Add `ssdp-client` crate, search for `urn:lge-com:service:webostv:1`, add `DiscoverTvs` D-Bus method.

### PolicyKit Authorization

**Goal**: Require authentication for privileged operations (pairing, removing TVs).

Implementation: Create policy file at `polkit/org.orlgtv.policy`, add authorization checks in D-Bus methods.

---

## Phase 3: User Session Service (`orlgtv-session`)

**Goal**: React to desktop events (screen lock/unlock, idle, suspend/resume).

### D-Bus Signals to Monitor

- `org.freedesktop.ScreenSaver.ActiveChanged` - screen lock/unlock
- `org.freedesktop.login1.Session.Lock`/`Unlock` - systemd-logind
- `org.freedesktop.login1.Manager.PrepareForSleep` - suspend/resume

### User Configuration (`~/.config/orlgtv/config.toml`)

```toml
[session]
on_screen_lock = "screen_off"     # screen_off | power_off | none
on_screen_unlock = "screen_on"    # screen_on | power_on | none
on_sleep = "power_off"
on_wake = "power_on"
```

### Systemd User Service

```ini
[Unit]
Description=orlgtv Session Service
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/bin/orlgtv-session
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

---

## Phase 4: System Tray (`orlgtv-tray`)

**Goal**: StatusNotifierItem tray icon for quick TV control using `ksni`.

Features:
- Connection status icons per TV
- Power/screen on/off
- Input switcher submenu
- Volume/backlight sliders
- Subscribe to `TvStateChanged` signal for live updates

---

## Phase 5: GUI Configuration (`orlgtv-gui`)

**Goal**: Slint-based configuration UI.

Features:
- TV discovery wizard (SSDP + manual IP)
- Pairing flow with TV prompt
- Per-TV configuration (name, MAC, default input)
- Session behavior settings

---

## Testing Commands

```bash
# Start daemon (development)
orlgtvd --session

# D-Bus operations
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ListTvs
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 PowerOn s "tv-id"
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ScreenOff s "tv-id"

# Monitor signals
busctl --user monitor org.orlgtv.Daemon

# Introspect interface
busctl --user introspect org.orlgtv.Daemon /org/orlgtv/Daemon
```

---

## Priority

1. **High**: Phase 3 (session service) - desktop integration
2. **Medium**: SSDP discovery, CLI D-Bus migration
3. **Lower**: Phase 4 (tray), Phase 5 (GUI), PolicyKit
