# tisvlay - LG WebOS TV Control for Linux (Rust)

## Project Overview

A Rust utility to control LG WebOS TVs from a Linux desktop connected via HDMI. The user's Nvidia GPU does not support HDMI-CEC, so network-based control via the LG WebOS SSAP protocol is required.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)
- KDE Plasma desktop environment

## Core Objectives

1. **Power Management**: Turn TV on (via Wake-on-LAN) and off
2. **HDMI Input Switching**: Switch to the HDMI port connected to the desktop
3. **Screen Blanking**: Turn screen off/on for OLED burn-in protection
4. **Volume Control**: Adjust volume and mute
5. **Desktop Integration**: React to D-Bus events (screen lock/unlock, suspend/resume)

## Technical Approach

- **Protocol**: LG WebOS SSAP over WebSocket (ws://port 3000, wss://port 3001)
- **Authentication**: TV pairing prompt, persist client key locally
- **Wake-on-LAN**: Required for power-on (TV network may be inactive in standby)
- **Async**: Use tokio runtime for non-blocking I/O

## Implementation Status

### Phase 1: Core Library + CLI (COMPLETE)

- [x] `tisvlay-core`: WebSocket client, SSAP protocol, WoL, config parsing
- [x] `tisvlay-cli`: Power, screen, input, volume, backlight, status commands
- [x] TV pairing flow with client key persistence
- [x] Multi-TV configuration support

### Phase 2: System Daemon (COMPLETE)

- [x] `tisvlayd`: D-Bus service (`org.tisvlay.Daemon1`) - see [crates/tisvlayd/src/](crates/tisvlayd/src/)
- [x] TV connection manager with pooling, auto-reconnect, boot-time WoL
- [x] Full D-Bus API: ListTvs, DiscoverTvs, PairTv, RemoveTv, Power/Screen/Input/Volume/Backlight control
- [x] TvStateChanged signal emission on connect/disconnect
- [x] Config reload via SIGHUP or D-Bus ReloadConfig
- [x] Systemd service files - see [systemd/](systemd/)
- [x] SSDP discovery (`discovery` feature) - see [docs/firewall.md](docs/firewall.md) for firewall requirements

### Phase 3: User Session Service (COMPLETE)

- [x] `tisvlay-session`: User service monitoring D-Bus session events - see [crates/tisvlay-session/src/](crates/tisvlay-session/src/)
- [x] Screen lock/unlock detection via `org.freedesktop.ScreenSaver.ActiveChanged`
- [x] Suspend/resume detection via `org.freedesktop.login1.Manager.PrepareForSleep`
- [x] Configurable actions per event (screen_on/off, power_on/off, none)
- [x] Optional idle timeout with polling-based detection
- [x] SIGHUP for config reload
- [x] Systemd user service file - see [systemd/tisvlay-session.service](systemd/tisvlay-session.service)

### Phase 4-5: Future

- Phase 4: `tisvlay-tray` (ksni StatusNotifierItem)
- Phase 5: `tisvlay-gui` (Slint configuration UI)

## Current Project Structure

```
tisvlay/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── tisvlay-core/              # Core library
│   │   └── src/
│   │       ├── lib.rs            # Public API exports
│   │       ├── client.rs         # WebOsClient - high-level API
│   │       ├── connection.rs     # WebSocket with rustls TLS
│   │       ├── protocol.rs       # SSAP message types
│   │       ├── endpoints.rs      # API endpoint constants
│   │       ├── config.rs         # TOML config (XDG paths), SessionConfig
│   │       ├── discovery.rs      # SSDP TV discovery (feature-gated)
│   │       ├── state.rs          # TvState, PowerState
│   │       ├── wol.rs            # Wake-on-LAN
│   │       └── error.rs          # Error types
│   │
│   ├── tisvlay-cli/               # CLI tool
│   │   └── src/
│   │       └── main.rs           # clap-based CLI
│   │
│   ├── tisvlayd/                  # System daemon (Phase 2)
│   │   └── src/
│   │       ├── main.rs           # Entry point, signal handling
│   │       ├── dbus.rs           # D-Bus interface (org.tisvlay.Daemon1)
│   │       └── tv_manager.rs     # TV connection pool
│   │
│   └── tisvlay-session/           # User session service (Phase 3)
│       └── src/
│           ├── main.rs           # Entry point, event handling
│           ├── daemon_client.rs  # D-Bus client for tisvlayd
│           ├── session_monitor.rs # Screen lock/unlock, suspend/resume
│           ├── wayland_idle.rs   # Wayland greeter wake detection
│           └── idle.rs           # Idle timeout detection
│
├── systemd/
│   ├── tisvlayd.service           # Systemd system service
│   ├── tisvlay-session.service    # Systemd user service (Phase 3)
│   ├── org.tisvlay.Daemon.service # D-Bus activation service
│   └── org.tisvlay.Daemon.conf    # D-Bus policy configuration
│
├── docs/
│   ├── reference.md              # Protocol survey from prior art
│   ├── design.md                 # Full architecture design
│   └── firewall.md               # SSDP firewall configuration
│
└── CLAUDE.md                     # This file
```

## Key Implementation Details

### TLS/WebSocket
- Uses `rustls` (not native-tls) to avoid OpenSSL dependency
- Custom `InsecureCertVerifier` accepts LG's self-signed certificates
- Tries wss://port 3001 first, falls back to ws://port 3000

### Configuration
- User config: `~/.config/tisvlay/config.toml`
- System config (Phase 2): `/etc/tisvlay/config.toml`
- Client keys stored in config after TV pairing

### Session Config Example
```toml
[session]
on_screen_lock = "screen_off"    # screen_off | power_off | none
on_screen_unlock = "screen_on"   # screen_on | power_on | none
on_greeter_wake = "screen_on"    # unblank TV on lock screen interaction (Wayland only)
on_sleep = "power_off"
on_wake = "power_on"
idle_timeout_seconds = 300       # 0 to disable
on_idle_timeout = "screen_off"
```

See [docs/design.md](docs/design.md#phase-3-user-session-integration---complete) for greeter wake detection details.

### CLI Commands
```
tisvlay power on|off       # WoL for power on
tisvlay screen on|off      # OLED-safe blank
tisvlay input [ID] [--list]
tisvlay volume [LEVEL|+N|-N]
tisvlay mute [on|off|toggle]
tisvlay backlight [LEVEL]
tisvlay status
tisvlay pair <IP> --name NAME --mac MAC
tisvlay list
tisvlay remove <ID>
```

## Key Crates

| Purpose | Crate |
|---------|-------|
| Async runtime | `tokio` |
| WebSocket | `tokio-tungstenite` + `rustls` |
| JSON | `serde`, `serde_json` |
| Config | `toml`, `directories` |
| CLI | `clap` |
| D-Bus | `zbus` |
| Wayland | `wayland-client`, `wayland-protocols` |
| Errors | `thiserror`, `anyhow` |
| Logging | `tracing`, `tracing-subscriber` |
| WoL | `socket2` |

## Known Quirks

- Newer firmware requires wss:// (port 3001) with self-signed cert
- TV may ignore WoL during pixel refresh
- Need "TV On via Wi-Fi" enabled in TV settings
- ~1-5 second delay needed between WoL and first command
- TV closes WebSocket on standby; need reconnection logic

## Quick Reference

### Running the Daemon

```bash
# Development (session bus):
tisvlayd --session

# Production (system bus):
sudo tisvlayd
```

### Running the Session Service

```bash
# Development (with tisvlayd --session):
tisvlay-session --session-daemon

# Production (as systemd user service):
systemctl --user enable --now tisvlay-session
```

### Testing with busctl

```bash
# List TVs
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 ListTvs

# Control TV
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 PowerOn s "tv-id"
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 ScreenOff s "tv-id"
busctl --user call org.tisvlay.Daemon /org/tisvlay/Daemon org.tisvlay.Daemon1 SetMute sb "tv-id" true

# Monitor signals
busctl --user monitor org.tisvlay.Daemon
```

### Systemd Installation

```bash
sudo cp systemd/tisvlayd.service /etc/systemd/system/
sudo cp systemd/org.tisvlay.Daemon.service /usr/share/dbus-1/system-services/
sudo cp systemd/org.tisvlay.Daemon.conf /etc/dbus-1/system.d/
sudo systemctl daemon-reload
sudo systemctl enable --now tisvlayd
```

## Reference Documentation

- [docs/reference.md](docs/reference.md) - Protocol deep-dive, API endpoints, prior art analysis
- [docs/design.md](docs/design.md) - Full architecture, D-Bus interface, config schema, phases
