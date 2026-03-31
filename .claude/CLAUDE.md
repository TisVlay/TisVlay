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
5. **Desktop Integration**: React to D-Bus events (session lock/unlock, suspend/resume)

## Technical Approach

- **Architecture**: Vendor-agnostic core with pluggable backends (`tisvlay-lg`, future `tisvlay-samsung`, etc.)
- **LG Protocol**: WebOS SSAP over WebSocket (ws://port 3000, wss://port 3001)
- **Authentication**: TV pairing prompt, persist credentials in vendor-specific config section
- **Wake-on-LAN**: Required for power-on (TV network may be inactive in standby)
- **Async**: Use tokio runtime for non-blocking I/O

## Implementation Status

Phases 1–5C and the per-TV system profiles refactor are complete. See [docs/design.md](docs/design.md) for full phase history and details.

**Complete:** Phase 1 (Core + CLI) · Phase 2 (Daemon) · Phase 3 (Session Service) · Phase 4a (CLI D-Bus) · Phase 4b (Tray) · Phase 4C (Config Split) · NixOS Packaging · Phase 5A (`managed` flag + CLI profile parity) · Phase 5B (live managed filtering) · Phase 5C (`ObjectManager` bulk discovery) · Per-TV System Profiles Refactor

### Phase 5: Next — GUI Configuration

- `tisvlay-gui`: Slint-based settings window
- TV discovery and pairing wizard (with managed checkbox)
- Per-TV configuration and session behavior settings

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
- D-Bus: `AddTv(name, vendor, config_json, managed)` -- vendor-specific params as JSON; `managed` required, no default in library
- D-Bus: `ObjectManager` at `/org/tisvlay` -- `GetManagedObjects` replaces `GetTvs`
- D-Bus: Manager1 methods: `ListSystemProfiles`, `GetUserProfileDefault`, `ListUserProfiles` (no `SetDaemonOption`)
- D-Bus: Tv1 properties include `SystemProfile`; Session1 methods: `ListUserProfiles`, `GetTvUserProfile`, `SetTvUserProfile`, `GetEffectiveUserProfile`
- `DiscoveredTv` uses `address`/`hardware_id` (not `ip`/`mac`)
- Two profile types: system profiles (daemon lifecycle events, per-TV) and user profiles (session events, per-TV)
- Config shape: `[system.profiles.*]` for daemon events, `[user.profiles.*]` for session events, `profile.system`/`profile.user` for per-TV assignment

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
│   │       ├── config.rs         # TOML config (XDG paths), SystemProfileConfig, UserProfileConfig
│   │       ├── dbus_paths.rs     # D-Bus path constants and helpers
│   │       ├── dbus_client.rs    # D-Bus client for tisvlayd (feature-gated)
│   │       ├── logging.rs        # Shared logging init with journald (feature-gated)
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
│   │       ├── session_monitor.rs # Session lock/unlock detection
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
- **System config**: `/var/lib/tisvlay/config.toml` — TV definitions (with `profile.system` assignment), credentials, system profiles (`[system.profiles.*]`), admin baseline user profiles (`[user.profiles.*]`). Owned by `tisvlayd`.
- **User config**: `~/.config/tisvlay/config.toml` — user profile definitions (`[user.profiles.*]`), per-TV user profile assignments (`[[tv]]` with `profile.user`). Owned by `tisvlay-session`. Optional; admin baseline `user.profiles.default` inherited via D-Bus if absent.
- Both configs use `[[tv]]` array-of-tables syntax; single-file mode works because each parser ignores unknown fields
- `tisvlayd --config <PATH>` and `tisvlay-session --config <PATH>` override respective config paths
- Client keys stored in vendor-specific config section after TV pairing (system config only)
- `TvConfig.vendor_config` uses `#[serde(flatten)]` with `HashMap<String, toml::Value>` -- core has zero vendor knowledge

See [docs/design.md](docs/design.md) for full schema and config examples.

See [docs/design.md](docs/design.md) for the full CLI command reference.

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
| Logging | `tracing`, `tracing-subscriber`, `tracing-journald` |
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
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetSupportedVendors
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ListSystemProfiles
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetUserProfileDefault
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ListUserProfiles

# List all objects + properties (replaces GetTvs)
busctl --user call org.tisvlay.Daemon /org/tisvlay org.freedesktop.DBus.ObjectManager GetManagedObjects

# Add TV (name, vendor, config_json, managed)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 AddTv sssb \
  "Living Room" "lg" '{"ip":"192.168.1.100","mac":"AA:BB:CC:DD:EE:FF"}' true

# Per-TV interfaces (replace living_room with TV ID)
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Tv/living_room
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 Address
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 Connected
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 SystemProfile
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

## Linting & CI

All checks are defined in `prek.toml` (single source of truth). Hooks are ordered by priority:
- **0**: file checks (trailing whitespace, EOF, TOML/YAML validation, merge conflicts, large files)
- **1**: `cargo fmt`
- **2**: `cargo build --profile test --workspace --all-targets --all-features`
- **3**: `cargo clippy --profile test --workspace --all-targets --all-features`
- **4**: `cargo test --workspace --all-features`

Build and clippy use `--profile test` so `cargo test` reuses all build artifacts with zero recompilation.

```bash
# Run all checks (local pre-commit)
prek run -a

# Run only fast file checks + fmt (skip compilation)
prek run -a --skip cargo-build --skip cargo-clippy --skip cargo-test

# Run a specific hook
prek run -a cargo-clippy

# CI runs file checks via prek, then build/clippy/test directly for live output
```

## Reference Documentation

- [docs/design.md](docs/design.md) - Full architecture, D-Bus interface, config schema, phases
- [docs/firewall.md](docs/firewall.md) - SSDP discovery firewall configuration
