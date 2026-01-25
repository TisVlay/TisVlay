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
- [daemon_client.rs](../crates/tisvlay-session/src/daemon_client.rs) - D-Bus client for tisvlayd
- [session_monitor.rs](../crates/tisvlay-session/src/session_monitor.rs) - Screen lock/suspend detection
- [wayland_idle.rs](../crates/tisvlay-session/src/wayland_idle.rs) - Wayland greeter wake detection
- [idle.rs](../crates/tisvlay-session/src/idle.rs) - Idle timeout polling

See [design.md](design.md#phase-3-user-session-integration---complete) for greeter wake detection details.

**Testing**:
```bash
# Development mode (with tisvlayd --session)
tisvlay-session --session-daemon

# Install as user service
cp systemd/tisvlay-session.service ~/.config/systemd/user/
systemctl --user enable --now tisvlay-session
```

---

## Optional Enhancements

### CLI D-Bus Migration

**Current**: CLI connects directly to TVs via WebSocket.
**Goal**: CLI uses daemon via D-Bus, with `--direct` fallback.

Benefits: Centralized connection management, automatic reconnection, config handled by daemon.

Implementation: Add `zbus` dependency, create D-Bus client module, add `--direct` flag.

### PolicyKit Authorization

**Goal**: Require authentication for privileged operations (pairing, removing TVs).

Implementation: Create policy file at `polkit/org.tisvlay.policy`, add authorization checks in D-Bus methods.

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

1. **High**: ~~Phase 3 (session service)~~ - COMPLETE
2. **High**: ~~SSDP discovery~~ - COMPLETE
3. **Medium**: CLI D-Bus migration
4. **Lower**: Phase 4 (tray), Phase 5 (GUI), PolicyKit
