# orlgtv - LG WebOS TV Control for Linux (Rust)

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

- [x] `orlgtv-core`: WebSocket client, SSAP protocol, WoL, config parsing
- [x] `orlgtv-cli`: Power, screen, input, volume, backlight, status commands
- [x] TV pairing flow with client key persistence
- [x] Multi-TV configuration support

### Phase 2: System Daemon (COMPLETE)

- [x] `orlgtvd`: D-Bus service (`org.orlgtv.Daemon1`) - see [crates/orlgtvd/src/](crates/orlgtvd/src/)
- [x] TV connection manager with pooling, auto-reconnect, boot-time WoL
- [x] Full D-Bus API: ListTvs, PairTv, RemoveTv, Power/Screen/Input/Volume/Backlight control
- [x] TvStateChanged signal emission on connect/disconnect
- [x] Config reload via SIGHUP or D-Bus ReloadConfig
- [x] Systemd service files - see [systemd/](systemd/)

### Phase 3-5: Future

- Phase 3: `orlgtv-session` user service (D-Bus session events)
- Phase 4: `orlgtv-tray` (ksni StatusNotifierItem)
- Phase 5: `orlgtv-gui` (Slint configuration UI)

## Current Project Structure

```
orlgtv/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── orlgtv-core/              # Core library
│   │   └── src/
│   │       ├── lib.rs            # Public API exports
│   │       ├── client.rs         # WebOsClient - high-level API
│   │       ├── connection.rs     # WebSocket with rustls TLS
│   │       ├── protocol.rs       # SSAP message types
│   │       ├── endpoints.rs      # API endpoint constants
│   │       ├── config.rs         # TOML config (XDG paths)
│   │       ├── state.rs          # TvState, PowerState
│   │       ├── wol.rs            # Wake-on-LAN
│   │       └── error.rs          # Error types
│   │
│   ├── orlgtv-cli/               # CLI tool
│   │   └── src/
│   │       └── main.rs           # clap-based CLI
│   │
│   └── orlgtvd/                  # System daemon (Phase 2)
│       └── src/
│           ├── main.rs           # Entry point, signal handling
│           ├── dbus.rs           # D-Bus interface (org.orlgtv.Daemon1)
│           └── tv_manager.rs     # TV connection pool
│
├── systemd/
│   ├── orlgtvd.service           # Systemd service unit
│   ├── org.orlgtv.Daemon.service # D-Bus activation service
│   └── org.orlgtv.Daemon.conf    # D-Bus policy configuration
│
├── docs/
│   ├── reference.md              # Protocol survey from prior art
│   └── design.md                 # Full architecture design
│
└── CLAUDE.md                     # This file
```

## Key Implementation Details

### TLS/WebSocket
- Uses `rustls` (not native-tls) to avoid OpenSSL dependency
- Custom `InsecureCertVerifier` accepts LG's self-signed certificates
- Tries wss://port 3001 first, falls back to ws://port 3000

### Configuration
- User config: `~/.config/orlgtv/config.toml`
- System config (Phase 2): `/etc/orlgtv/config.toml`
- Client keys stored in config after TV pairing

### CLI Commands
```
orlgtv power on|off       # WoL for power on
orlgtv screen on|off      # OLED-safe blank
orlgtv input [ID] [--list]
orlgtv volume [LEVEL|+N|-N]
orlgtv mute [on|off|toggle]
orlgtv backlight [LEVEL]
orlgtv status
orlgtv pair <IP> --name NAME --mac MAC
orlgtv list
orlgtv remove <ID>
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
orlgtvd --session

# Production (system bus):
sudo orlgtvd
```

### Testing with busctl

```bash
# List TVs
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ListTvs

# Control TV
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 PowerOn s "tv-id"
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ScreenOff s "tv-id"
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 SetMute sb "tv-id" true

# Monitor signals
busctl --user monitor org.orlgtv.Daemon
```

### Systemd Installation

```bash
sudo cp systemd/orlgtvd.service /etc/systemd/system/
sudo cp systemd/org.orlgtv.Daemon.service /usr/share/dbus-1/system-services/
sudo cp systemd/org.orlgtv.Daemon.conf /etc/dbus-1/system.d/
sudo systemctl daemon-reload
sudo systemctl enable --now orlgtvd
```

## Reference Documentation

- [docs/reference.md](docs/reference.md) - Protocol deep-dive, API endpoints, prior art analysis
- [docs/design.md](docs/design.md) - Full architecture, D-Bus interface, config schema, phases
