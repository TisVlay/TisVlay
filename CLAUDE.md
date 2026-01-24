# orlgtv - LG WebOS TV Control for Linux (Rust)

## Project Overview

A Rust utility to control LG WebOS TVs from a Linux desktop connected via HDMI. The user's Nvidia GPU does not support HDMI-CEC, so network-based control via the LG WebOS SSAP protocol is required.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)

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

## Key Crates

| Purpose | Crate |
|---------|-------|
| Async runtime | `tokio` |
| WebSocket | `tokio-tungstenite` |
| JSON | `serde`, `serde_json` |
| D-Bus | `zbus` |
| CLI | `clap` |
| Config storage | `directories` |

## Project Structure

```
orlgtv/
├── src/
│   ├── lib.rs           # Public API
│   ├── client.rs        # WebOsClient
│   ├── connection.rs    # WebSocket management
│   ├── protocol.rs      # SSAP types
│   ├── endpoints.rs     # API endpoints
│   ├── wol.rs           # Wake-on-LAN
│   ├── error.rs         # Error types
│   └── state.rs         # TV state model
├── docs/
│   └── reference.md     # Detailed prior art survey
└── Cargo.toml
```

## Feature Flags

```toml
[features]
default = ["cli"]
cli = ["clap"]
daemon = ["zbus"]        # D-Bus integration for desktop events
discovery = []           # SSDP TV discovery
```

## Known Quirks

- Newer firmware requires wss:// (port 3001) with self-signed cert
- TV may ignore WoL during pixel refresh
- Need "TV On via Wi-Fi" enabled in TV settings
- ~1-5 second delay needed between WoL and first command
- TV closes WebSocket on standby; need reconnection logic

## Reference Documentation

See [docs/reference.md](docs/reference.md) for detailed protocol documentation, API endpoints, and analysis of existing implementations (LGTVCompanion, bscpylgtv, aiowebostv, etc.).
