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

### Phase 2: System Daemon (NEXT)

- [ ] `orlgtvd`: Systemd system service with D-Bus API
- [ ] TV connection management with reconnection logic
- [ ] Boot-time WoL
- [ ] SSDP discovery

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
│   └── orlgtv-cli/               # CLI tool
│       └── src/
│           └── main.rs           # clap-based CLI
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
| Errors | `thiserror`, `anyhow` |
| Logging | `tracing`, `tracing-subscriber` |
| WoL | `socket2` |

## Known Quirks

- Newer firmware requires wss:// (port 3001) with self-signed cert
- TV may ignore WoL during pixel refresh
- Need "TV On via Wi-Fi" enabled in TV settings
- ~1-5 second delay needed between WoL and first command
- TV closes WebSocket on standby; need reconnection logic

## Phase 2 Implementation Notes

For the system daemon (`orlgtvd`):

1. **D-Bus Interface**: See `docs/design.md` for full interface spec
   - Service: `org.orlgtv.Daemon`
   - Object: `/org/orlgtv/Daemon`
   - Interface: `org.orlgtv.Daemon1`

2. **Key Methods**:
   - PowerOn/PowerOff, ScreenOn/ScreenOff
   - SwitchInput, SetVolume, SetBacklight
   - PairTv, RemoveTv, ListTvs, DiscoverTvs

3. **Dependencies to Add**:
   - `zbus = "4"` for D-Bus
   - `ssdp-client` for TV discovery (optional feature)

4. **User Preferences**: KDE Plasma, D-Bus system bus for IPC, PolicyKit for auth

## Reference Documentation

- [docs/reference.md](docs/reference.md) - Protocol deep-dive, API endpoints, prior art analysis
- [docs/design.md](docs/design.md) - Full architecture, D-Bus interface, config schema, phases
