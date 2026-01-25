# Future Work

## Phase 2 Status: COMPLETE

All core Phase 2 functionality is implemented in `crates/tisvlayd/`. See source code for details:
- [dbus.rs](../crates/tisvlayd/src/dbus.rs) - D-Bus interface (`org.tisvlay.Daemon1`)
- [tv_manager.rs](../crates/tisvlayd/src/tv_manager.rs) - Connection management, reconnection, state events
- [main.rs](../crates/tisvlayd/src/main.rs) - Signal handling, D-Bus setup

**SSDP Discovery**: Implemented behind `discovery` feature flag. See [firewall.md](firewall.md) for Linux firewall requirements.

---

## Phase 3 Status: COMPLETE

User session service implemented in `crates/tisvlay-session/`. See source code for details:
- [main.rs](../crates/tisvlay-session/src/main.rs) - Entry point, event handling, signal handling
- [session_monitor.rs](../crates/tisvlay-session/src/session_monitor.rs) - Screen lock/suspend detection
- [wayland_idle.rs](../crates/tisvlay-session/src/wayland_idle.rs) - Wayland greeter wake detection
- [idle.rs](../crates/tisvlay-session/src/idle.rs) - Idle timeout polling

See [design.md](design.md#phase-3-user-session-integration---complete) for greeter wake detection details.

**Testing**:
```bash
# Development mode (with tisvlayd --session)
tisvlay-session --user

# Install as user service
cp systemd/tisvlay-session.service ~/.config/systemd/user/
systemctl --user enable --now tisvlay-session
```

---

## Phase 4a Status: COMPLETE

CLI D-Bus migration implemented. See source code for details:
- [dbus_client.rs](../crates/tisvlay-core/src/dbus_client.rs) - Shared D-Bus client (feature-gated: `dbus`)
- [main.rs](../crates/tisvlay-cli/src/main.rs) - CLI with D-Bus and direct modes

**Features**:
- CLI uses D-Bus by default to communicate with `tisvlayd`
- `--no-daemon` flag for direct WebSocket connections (troubleshooting/development)
- `--user` flag for development with `tisvlayd --session`
- Shared `DaemonClient` in `tisvlay-core` used by both CLI and session service

**Testing**:
```bash
# D-Bus mode (default) - requires tisvlayd running
tisvlay list
tisvlay screen off

# Development mode (with tisvlayd --session)
tisvlay --user list

# Direct mode (no daemon needed)
tisvlay --no-daemon list
tisvlay --no-daemon screen off
```

---

## Deferred Tasks

### User Configuration Documentation

**Goal**: Document all possible user configuration behaviors and settings.

Scope:
- Session event actions (`on_screen_lock`, `on_screen_unlock`, `on_greeter_wake`, etc.)
- Idle timeout configuration
- Per-TV settings (future)
- Tray preferences (future)

**Status**: Deferred to later phase (after tray/GUI work clarifies requirements).

### Packaging and Distribution

**Goal**: Provide native packages for major Linux distributions.

Scope:
- Debian/Ubuntu `.deb` packages
- Fedora/RHEL `.rpm` packages
- Arch Linux `PKGBUILD`
- NixOS flake/package

Considerations:
- System daemon requires native packaging (systemd, D-Bus, polkit integration)
- Flatpak limitations: cannot install systemd services, no raw sockets for WoL
- Split packages: `tisvlay` (daemon + CLI) vs `tisvlay-tray` vs `tisvlay-gui`
- `cargo install` provides basic functionality; full system integration requires native packages

**Status**: Deferred until core functionality stabilizes.

### PolicyKit Authorization

**Goal**: Require authentication for privileged operations (pairing, removing TVs).

Implementation: Create policy file at `polkit/org.tisvlay.policy`, add authorization checks in D-Bus methods.

**Status**: Deferred to Phase 5 (GUI) or later.

---

## Phase 4: System Tray (`tisvlay-tray`)

**Goal**: StatusNotifierItem tray icon for quick TV control using `ksni`.

Features:
- Connection status icons per TV
- Power/screen on/off
- Input switcher submenu
- Volume/backlight sliders
- Subscribe to `TvStateChanged` signal for live updates

---

## Phase 5: GUI Configuration (`tisvlay-gui`)

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
tisvlayd --session

# Start session service (development, in another terminal)
tisvlay-session --session-daemon

# D-Bus operations
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 ListTvs
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 PowerOn s "tv-id"
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 ScreenOff s "tv-id"

# Discover TVs (requires firewall config - see docs/firewall.md)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 DiscoverTvs u 3000

# Monitor signals
busctl --user monitor org.tisvlay.Daemon

# Introspect interface
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Daemon

# Test screen lock detection (KDE)
loginctl lock-session

# Test screen lock detection (freedesktop)
qdbus org.freedesktop.ScreenSaver /ScreenSaver Lock
```

---

## Priority

1. ~~Phase 1 (core library + CLI)~~ - COMPLETE
2. ~~Phase 2 (system daemon)~~ - COMPLETE
3. ~~Phase 3 (session service)~~ - COMPLETE
4. ~~Phase 4a (CLI D-Bus migration)~~ - COMPLETE
5. **Next**: Phase 4b (tray app)
6. **Later**: Phase 5 (GUI), user config docs, packaging, PolicyKit
