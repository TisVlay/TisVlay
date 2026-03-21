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

- [x] CLI communicates with daemon via D-Bus (daemon-only, no direct mode)
- [x] `--user` flag for user/session bus (development mode)
- [x] Shared `DaemonClient` in `tisvlay-core` (feature-gated: `dbus`)
- [x] CLI depends only on `tisvlay-core`, not vendor crates

### Phase 4b: System Tray App (COMPLETE)

- [x] `tisvlay-tray`: StatusNotifierItem tray icon using `ksni` - see [crates/tisvlay-tray/src/](crates/tisvlay-tray/src/)
- [x] Per-TV submenus with power, screen blank, input switching, volume, backlight, mute controls
- [x] Scroll wheel adjusts volume or backlight (context-dependent)
- [x] Optimistic state caching to prevent race conditions on rapid adjustments
- [x] Polling-based state refresh (5s interval) with short timeouts for disconnected TVs
- [x] Graceful degradation when daemon is unavailable
- [x] Signal handling (SIGINT/SIGTERM) for clean shutdown
- [x] Systemd user service file - see [systemd/tisvlay-tray.service](systemd/tisvlay-tray.service)

### Phase 4C: System vs User Config Split (COMPLETE)

- [x] `SystemConfig` (daemon-owned: `/var/lib/tisvlay/config.toml`) split from `UserConfig` (session-owned: `~/.config/tisvlay/config.toml`)
- [x] `ProfileConfig` field-level inheritance: user profile fields override system defaults; unset fields fall through to system default, then hard-coded defaults
- [x] `on_idle_resume: Option<SessionAction>` added to `ProfileConfig` (default `ScreenOn`)
- [x] `org.tisvlay.Session1` D-Bus interface: `ListProfiles`, `GetTvProfile`, `SetTvProfile`, `GetEffectiveProfile`, `ReloadConfig`
- [x] Daemon logind monitoring: `PrepareForShutdown` (inhibitor lock), `SessionNew`/`SessionRemoved` (`on_no_active_session`)
- [x] Boot-wake refactored to retry-connect loop (WoL resend on failure, ~10 retries)
- [x] Session service event replay after late daemon connect
- [x] Tray Profile submenu per TV; Refresh button reloads daemon + session configs
- [x] Daemon Manager1 additions: `SetDaemonOption`, `SetTvOption`, `GetSystemProfile`, `GetSystemProfiles`
- [x] `SessionClient` in `tisvlay-core` for session service D-Bus communication

### NixOS Packaging (COMPLETE)

- [x] `nix/package.nix`: `rustPlatform.buildRustPackage` for all binaries
- [x] `nix/module.nix`: `services.tisvlay.enable` NixOS module
- [x] `flake.nix`: `nixosModules.default`, `overlays.default`, `packages.default`
- [x] `tisvlayd --config <PATH>` flag; default `/var/lib/tisvlay/config.toml` for system daemon
- [x] `tisvlay-session --config <PATH>` flag; default `~/.config/tisvlay/config.toml` (user config)
- [x] Dedicated `tisvlay` system user/group with `StateDirectory`
- [x] D-Bus policy via `services.dbus.packages` (idiomatic NixOS)
- [x] User services (`tisvlay-session`, `tisvlay-tray`) auto-start on `graphical-session.target`
- [x] MSRV lowered to 1.93 to match nixpkgs bundled Rust

### Phase 5: Future

- Phase 5: `tisvlay-gui` (Slint configuration UI)

### Multi-Vendor Refactor (COMPLETE)

Architecture refactored to support multiple TV vendors. Daemon and CLI are fully vendor-agnostic and transport-agnostic.

**Design Documents**:
- [docs/design.md](docs/design.md) - Full architecture specification

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
│   │       ├── traits.rs         # TvBackend, TvConnection, TvConnector, TvDiscovery
│   │       ├── capabilities.rs   # Capability bitflags
│   │       ├── pairing.rs        # Unified pairing state machine
│   │       ├── config.rs         # TOML config (XDG paths), ProfileConfig
│   │       ├── dbus_paths.rs     # D-Bus path constants and helpers
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
│   │       ├── dbus/             # D-Bus interfaces (Manager1, Tv1.*)
│   │       ├── logind.rs         # logind monitoring (shutdown inhibitor, session tracking)
│   │       ├── registry.rs       # Re-exports from tisvlay-backends
│   │       └── tv_manager.rs     # TV connection pool, disk serialization
│   │
│   ├── tisvlay-session/           # User session service
│   │   └── src/
│   │       ├── main.rs           # Entry point, event handling
│   │       ├── session_dbus.rs   # org.tisvlay.Session1 D-Bus interface
│   │       ├── session_monitor.rs # Screen lock/unlock, suspend/resume
│   │       ├── wayland_idle.rs   # Wayland greeter wake detection
│   │       └── idle.rs           # Idle timeout detection
│   │
│   └── tisvlay-tray/              # System tray app
│       └── src/
│           ├── main.rs           # Entry point, polling, command processor
│           ├── tray.rs           # ksni Tray impl, menu building, TrayCommand
│           └── state.rs          # CachedTvState, fetch_all_state()
│
├── systemd/
│   ├── tisvlayd.service           # Systemd system service
│   ├── tisvlay-session.service    # Systemd user service
│   ├── tisvlay-tray.service       # Systemd user service (tray)
│   ├── org.tisvlay.Daemon.service # D-Bus activation service
│   └── org.tisvlay.Daemon.conf    # D-Bus policy configuration
│
├── nix/
│   ├── package.nix               # Nix package derivation (rustPlatform.buildRustPackage)
│   └── module.nix                # NixOS module (services.tisvlay.enable)
│
├── docs/
│   ├── design.md                 # Full architecture design
│   └── firewall.md               # SSDP firewall configuration
│
└── .claude/CLAUDE.md             # This file
```

## Key Implementation Details

### Transport-Agnostic Architecture
- `TvConnector::connect(&self, vendor_config: &toml::Value)` -- connectors own all transport details
- `TvConnection` has all feature methods directly with default not-supported impls
- `BackendRegistry` provides `connect_tv()`, `display_address()`, `supports_wake()`, `wake_tv()`, `start_pairing()`, `build_config_from_pairing()`
- Daemon depends on `tisvlay-backends`; CLI depends only on `tisvlay-core` (daemon-only)
- Adding a new vendor: create crate, add to `tisvlay-backends/Cargo.toml`, register in `create_registry()`

### TLS/WebSocket (LG backend)
- Uses `rustls` (not native-tls) to avoid OpenSSL dependency
- Custom `InsecureCertVerifier` accepts LG's self-signed certificates
- Tries wss://port 3001 first, falls back to ws://port 3000

### Configuration
- **System config**: `/var/lib/tisvlay/config.toml` — TV definitions, credentials, daemon settings, system default profile. Owned by `tisvlayd`.
- **User config**: `~/.config/tisvlay/config.toml` — profile definitions, per-TV profile assignments. Owned by `tisvlay-session`. Optional; system default profile inherited via D-Bus if absent.
- `tisvlayd --config <PATH>` and `tisvlay-session --config <PATH>` override respective config paths
- Client keys stored in vendor-specific config section after TV pairing (system config only)
- `TvConfig.vendor_config` uses `#[serde(flatten)]` with `HashMap<String, toml::Value>` -- core has zero vendor knowledge

See [docs/design.md](docs/design.md) for full schema and config examples.

### CLI Commands
```
# CLI communicates with tisvlayd via D-Bus
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
| System tray | `ksni` |
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

### Running the Tray App

```bash
# Development (with tisvlayd --session):
tisvlay-tray --user

# Production (as systemd user service):
systemctl --user enable --now tisvlay-tray
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

### NixOS Installation

```nix
# flake.nix input
inputs.tisvlay.url = "github:ruffsl/tisvlay";

# configuration.nix
{ inputs, ... }: {
  imports = [ inputs.tisvlay.nixosModules.default ];
  services.tisvlay.enable = true;
}
```

This sets up:
- `tisvlayd` system service (runs as `tisvlay` user, config in `/var/lib/tisvlay/`)
- `tisvlay-session` user service (auto-starts on graphical session)
- `tisvlay-tray` user service (auto-starts on graphical session)
- D-Bus policy and CLI in `$PATH`

### Manual Systemd Installation

```bash
sudo cp systemd/tisvlayd.service /etc/systemd/system/
sudo cp systemd/org.tisvlay.Daemon.service /usr/share/dbus-1/system-services/
sudo cp systemd/org.tisvlay.Daemon.conf /etc/dbus-1/system.d/
sudo systemctl daemon-reload
sudo systemctl enable --now tisvlayd
```

## Reference Documentation

- [docs/design.md](docs/design.md) - Full architecture, D-Bus interface, config schema, phases
- [docs/firewall.md](docs/firewall.md) - SSDP discovery firewall configuration
