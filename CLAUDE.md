# tisvlay - Multi-Vendor TV Control for Linux (Rust)

## Project Overview

A Rust utility to control smart TVs from a Linux desktop connected via HDMI. Currently supports LG WebOS TVs, with a vendor-agnostic architecture designed for adding Samsung, Sony, and other backends. The user's Nvidia GPU does not support HDMI-CEC, so network-based control via vendor protocols (e.g., LG WebOS SSAP) is required.

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

- **Architecture**: Vendor-agnostic core with pluggable backends (`tisvlay-lg`, future `tisvlay-samsung`, etc.)
- **LG Protocol**: WebOS SSAP over WebSocket (ws://port 3000, wss://port 3001)
- **Authentication**: TV pairing prompt, persist credentials in vendor-specific config section
- **Wake-on-LAN**: Required for power-on (TV network may be inactive in standby)
- **Async**: Use tokio runtime for non-blocking I/O

## Implementation Status

### Phase 1: Core Library + CLI (COMPLETE)

- [x] `tisvlay-core`: Vendor-agnostic traits, capabilities, WoL, config parsing
- [x] `tisvlay-lg`: LG WebOS backend (WebSocket client, SSAP protocol)
- [x] `tisvlay-backends`: Backend registry (composition point for all vendors)
- [x] `tisvlay-cli`: Power, screen, input, volume, backlight, status commands
- [x] TV pairing flow with credential persistence
- [x] Multi-TV configuration support with profile-based session behavior

### Phase 2: System Daemon (COMPLETE)

- [x] `tisvlayd`: D-Bus service (`org.tisvlay.Manager1` + per-TV `org.tisvlay.Tv1.*`) - see [crates/tisvlayd/src/](crates/tisvlayd/src/)
- [x] Vendor-agnostic TV connection manager with pooling, auto-reconnect, boot-time WoL
- [x] D-Bus Manager API: GetTvs, GetSupportedVendors, AddTv, RemoveTv, ReloadConfig, DiscoverTvs
- [x] Per-TV D-Bus objects at `/org/tisvlay/Tv/{id}` with Power/Screen/Volume/Input/Backlight interfaces
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

### Phase 4a: CLI D-Bus Migration (COMPLETE)

- [x] CLI uses D-Bus by default to communicate with daemon
- [x] `--no-daemon` flag for direct connections via backend registry (vendor-agnostic)
- [x] `--user` flag for user/session bus (development mode)
- [x] Shared `DaemonClient` in `tisvlay-core` (feature-gated: `dbus`)
- [x] CLI depends only on `tisvlay-backends`, not vendor crates directly

### Phase 4b-5: Future

- Phase 4b: `tisvlay-tray` (ksni StatusNotifierItem)
- Phase 5: `tisvlay-gui` (Slint configuration UI)

### Multi-Vendor Refactor (COMPLETE)

Architecture refactored to support multiple TV vendors. Daemon and CLI are fully vendor-agnostic and transport-agnostic.

**Design Documents**:
- [docs/design.md](docs/design.md) - Full architecture specification
- [docs/multi-vendor-discussion.md](docs/multi-vendor-discussion.md) - Design rationale and Q&A

**Key Architectural Decisions**:
- Separate crates per vendor: `tisvlay-lg` (only `tisvlay-backends` imports vendor crates)
- Core traits in `tisvlay-core`: `TvBackend`, `TvConnection` (with flattened feature methods), `TvConnector`
- `TvConnector::connect()` takes `&toml::Value` -- fully transport-agnostic
- `TvConnection` has all feature methods with default not-supported impls (no downcasting needed)
- `BackendRegistry` in `tisvlay-backends` provides `connect_tv()`, `wake_tv()`, `start_pairing()`, etc.
- Capability bitflags (`Capabilities::ALL`) for runtime feature detection
- D-Bus: per-TV objects at `/org/tisvlay/Tv/{id}` with `Address` property (not `Ip`)
- D-Bus: `AddTv(name, vendor, config_json)` -- vendor-specific params as JSON
- `DiscoveredTv` uses `address`/`hardware_id` (not `ip`/`mac`)
- Profile-based config for multi-TV setups with display-aware control

## Current Project Structure

```
tisvlay/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── tisvlay-core/              # Core library (vendor-agnostic traits & types)
│   │   └── src/
│   │       ├── lib.rs            # Public API exports
│   │       ├── traits.rs         # TvBackend, TvConnection, feature traits
│   │       ├── capabilities.rs   # Capability bitflags
│   │       ├── pairing.rs        # Unified pairing state machine
│   │       ├── config.rs         # TOML config (XDG paths), ProfileConfig
│   │       ├── dbus_client.rs    # D-Bus client for tisvlayd (feature-gated)
│   │       ├── state.rs          # TvState, PowerState, InputInfo
│   │       ├── wol.rs            # Wake-on-LAN
│   │       └── error.rs          # Error types
│   │
│   ├── tisvlay-lg/                # LG WebOS vendor implementation
│   │   └── src/
│   │       ├── lib.rs            # Public exports
│   │       ├── backend.rs        # LgConnector, LgConnection trait impls
│   │       ├── client.rs         # WebOsClient - high-level API
│   │       ├── connection.rs     # WebSocket with rustls TLS
│   │       ├── protocol.rs       # SSAP message types
│   │       ├── endpoints.rs      # API endpoint constants
│   │       ├── config.rs         # LgVendorConfig
│   │       └── discovery.rs      # SSDP TV discovery (feature-gated)
│   │
│   ├── tisvlay-backends/          # Backend registry (composition point)
│   │   └── src/
│   │       └── lib.rs            # BackendRegistry, vendor registration
│   │
│   ├── tisvlay-cli/               # CLI tool
│   │   └── src/
│   │       └── main.rs           # clap-based CLI
│   │
│   ├── tisvlayd/                  # System daemon
│   │   └── src/
│   │       ├── main.rs           # Entry point, signal handling
│   │       ├── dbus.rs           # D-Bus interface (org.tisvlay.Daemon1)
│   │       ├── registry.rs       # Re-exports from tisvlay-backends
│   │       └── tv_manager.rs     # TV connection pool
│   │
│   └── tisvlay-session/           # User session service
│       └── src/
│           ├── main.rs           # Entry point, event handling
│           ├── session_monitor.rs # Screen lock/unlock, suspend/resume
│           ├── wayland_idle.rs   # Wayland greeter wake detection
│           └── idle.rs           # Idle timeout detection
│
├── systemd/
│   ├── tisvlayd.service           # Systemd system service
│   ├── tisvlay-session.service    # Systemd user service
│   ├── org.tisvlay.Daemon.service # D-Bus activation service
│   └── org.tisvlay.Daemon.conf    # D-Bus policy configuration
│
├── docs/
│   ├── reference.md              # Protocol survey from prior art
│   ├── design.md                 # Full architecture design
│   ├── firewall.md               # SSDP firewall configuration
│   └── multi-vendor-discussion.md # Design rationale and Q&A
│
└── CLAUDE.md                     # This file
```

## Key Implementation Details

### Transport-Agnostic Architecture
- `TvConnector::connect(&self, vendor_config: &toml::Value)` -- connectors own all transport details
- `TvConnection` has all feature methods directly with default not-supported impls
- `BackendRegistry` provides `connect_tv()`, `display_address()`, `supports_wake()`, `wake_tv()`, `start_pairing()`, `build_config_from_pairing()`
- Daemon and CLI depend only on `tisvlay-backends`, never on vendor crates directly
- Adding a new vendor: create crate, add to `tisvlay-backends/Cargo.toml`, register in `create_registry()`

### TLS/WebSocket (LG backend)
- Uses `rustls` (not native-tls) to avoid OpenSSL dependency
- Custom `InsecureCertVerifier` accepts LG's self-signed certificates
- Tries wss://port 3001 first, falls back to ws://port 3000

### Configuration
- User config: `~/.config/tisvlay/config.toml`
- System config: `/etc/tisvlay/config.toml`
- Client keys stored in vendor-specific config section after TV pairing
- `TvConfig.vendor_config` uses `#[serde(flatten)]` with `HashMap<String, toml::Value>` -- core has zero vendor knowledge

### Profile Config Example
```toml
[profiles.default]
on_screen_lock = "screen_off"    # screen_off | power_off | none
on_screen_unlock = "screen_on"   # screen_on | power_on | none
on_greeter_wake = "screen_on"    # unblank TV on lock screen interaction (Wayland only)
on_sleep = "power_off"
on_wake = "power_on"
idle_timeout_seconds = 300       # 0 to disable
on_idle_timeout = "screen_off"

[profiles.gaming]
on_screen_lock = "none"          # Don't interrupt gameplay
idle_timeout_seconds = 0         # Disable idle timeout

[[tv]]
id = "living-room"
name = "Living Room TV"
vendor = "lg"
profile = "gaming"               # Reference profile by name

# Vendor-specific settings including connection parameters
# All connection params (ip, mac, etc.) are vendor-specific
[tv.lg]
ip = "192.168.1.100"             # Required for LG WebOS TVs
mac = "AA:BB:CC:DD:EE:FF"        # For Wake-on-LAN
client_key = "abc123"
default_input = "HDMI_1"
```

See [docs/design.md](docs/design.md#phase-3-user-session-integration---complete) for greeter wake detection details.

### CLI Commands
```
# By default, CLI uses D-Bus to communicate with tisvlayd
tisvlay power on|off       # WoL for power on
tisvlay screen on|off      # OLED-safe blank
tisvlay input [ID] [--list]
tisvlay volume [LEVEL|+N|-N]
tisvlay mute [on|off|toggle]
tisvlay backlight [LEVEL]
tisvlay status
tisvlay pair <ADDRESS> --name NAME --vendor lg --wake-id MAC
tisvlay list
tisvlay remove <ID>

# Global flags
tisvlay --no-daemon <cmd>        # Direct connection via backend registry (no daemon)
tisvlay --user <cmd>             # Use user/session bus (development)
tisvlay --tv <ID> <cmd>          # Target specific TV
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
tisvlay-session --user

# Production (as systemd user service):
systemctl --user enable --now tisvlay-session
```

### Testing with busctl

```bash
# Manager interface
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetTvs
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetSupportedVendors
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig

# Add TV (name, vendor, config_json)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 AddTv sss \
  "Living Room" "lg" '{"ip":"192.168.1.100","mac":"AA:BB:CC:DD:EE:FF"}'

# Per-TV interfaces (replace living_room with TV ID)
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Tv/living_room
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 Address
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 Connected
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Power PowerOff
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Screen ScreenOff
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume SetMute b true

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
