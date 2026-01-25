# Future Work

## Phase 2 Status: COMPLETE

All core Phase 2 functionality is implemented in `crates/orlgtvd/`. See source code for details:
- [dbus.rs](../crates/orlgtvd/src/dbus.rs) - D-Bus interface (`org.orlgtv.Daemon1`)
- [tv_manager.rs](../crates/orlgtvd/src/tv_manager.rs) - Connection management, reconnection, state events
- [main.rs](../crates/orlgtvd/src/main.rs) - Signal handling, D-Bus setup

**SSDP Discovery**: Implemented behind `discovery` feature flag. See [firewall.md](firewall.md) for Linux firewall requirements.

---

## Phase 3 Status: COMPLETE

User session service implemented in `crates/orlgtv-session/`. See source code for details:
- [main.rs](../crates/orlgtv-session/src/main.rs) - Entry point, event handling, signal handling
- [daemon_client.rs](../crates/orlgtv-session/src/daemon_client.rs) - D-Bus client for orlgtvd
- [session_monitor.rs](../crates/orlgtv-session/src/session_monitor.rs) - Screen lock/suspend detection
- [wayland_idle.rs](../crates/orlgtv-session/src/wayland_idle.rs) - Wayland greeter wake detection
- [idle.rs](../crates/orlgtv-session/src/idle.rs) - Idle timeout polling

See [design.md](design.md#phase-3-user-session-integration---complete) for greeter wake detection details.

**Testing**:
```bash
# Development mode (with orlgtvd --session)
orlgtv-session --session-daemon

# Install as user service
cp systemd/orlgtv-session.service ~/.config/systemd/user/
systemctl --user enable --now orlgtv-session
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

Implementation: Create policy file at `polkit/org.orlgtv.policy`, add authorization checks in D-Bus methods.

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

# Start session service (development, in another terminal)
orlgtv-session --session-daemon

# D-Bus operations
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ListTvs
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 PowerOn s "tv-id"
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ScreenOff s "tv-id"

# Discover TVs (requires firewall config - see docs/firewall.md)
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 DiscoverTvs u 3000

# Monitor signals
busctl --user monitor org.orlgtv.Daemon

# Introspect interface
busctl --user introspect org.orlgtv.Daemon /org/orlgtv/Daemon

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
