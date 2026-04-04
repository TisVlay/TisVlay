# tisvlay - Multi-Vendor TV Control for Linux (Rust)

## Project Overview

A Rust utility to control smart TVs from a Linux desktop connected via HDMI. Currently supports LG WebOS TVs, with a vendor-agnostic architecture designed for adding Samsung, Sony, and other backends. The user's Nvidia GPU does not support HDMI-CEC, so network-based control via vendor protocols (e.g., LG WebOS SSAP) is required.

See [docs/design.md](docs/design.md) for the full architecture, D-Bus interface specs, config schema, CLI reference, and implementation phase history.

## Target Hardware

- **LG OLED evo C3 42"** (OLED42C3PUA) - 2023 model
- **LG G4 65"** (OLED65G4SUB) - 2024 model
- Linux desktop with Nvidia GPU (no HDMI-CEC support)
- KDE Plasma desktop environment

## What's Next

- `tisvlay-gui`: Slint-based settings window
- TV discovery and pairing wizard (with managed checkbox)
- Per-TV configuration and session behavior settings
- Input-aware automation (`primary_input`, `automate_when`) — see design.md § Power & Screen State Machine

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
│   │       ├── main.rs           # Entry point, connection::Builder, state_change_forwarder
│   │       ├── dbus/             # D-Bus interfaces (Manager1, Tv1.* with cached properties)
│   │       ├── logind.rs         # logind monitoring (shutdown inhibitor, session tracking)
│   │       ├── network_watcher.rs # NetworkManager D-Bus watcher (network-transport reconnection)
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
│           ├── main.rs           # Entry point, signal watcher, command processor
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
├── packaging/
│   ├── nix/
│   │   ├── package.nix           # Nix package derivation (rustPlatform.buildRustPackage)
│   │   └── module.nix            # NixOS module (services.tisvlay.enable)
│   └── scripts/
│       ├── postinstall.sh        # Distro package post-install (systemd + D-Bus reload)
│       └── preremove.sh          # Distro package pre-remove (stop services)
│
├── docs/
│   ├── design.md                 # Full architecture design
│   └── firewall.md               # SSDP firewall configuration
│
└── .claude/CLAUDE.md             # This file
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
| Logging | `tracing`, `tracing-subscriber`, `tracing-journald` |
| WoL | `socket2` |

## Known Quirks

- Newer firmware requires wss:// (port 3001) with self-signed cert; uses `rustls` with custom `InsecureCertVerifier`
- TV may ignore WoL during pixel refresh
- Need "TV On via Wi-Fi" enabled in TV settings
- ~1-5 second delay needed between WoL and first command
- TV closes WebSocket on standby; need reconnection logic
- Backlight IS SSAP-subscribable (via `getSystemSettings` with `category: "picture", keys: ["backlight"]`); writing still requires Luna `createAlert` workaround (direct setSystemSettings returns 401)
- SSAP subscription updates carry the original request ID, not the URI -- routing must use a request-ID-to-URI map
- Multiplexed WebSocket: background reader routes subscription updates to state channel and command responses to callers; SSAP subscriptions (power, volume, mute, inputs, foreground app, backlight) push state in real time
- LG pairing is two-phase: TV sends `Response` (prompt shown), then `Registered` (user accepted). `request_wait()` on the multiplexed WebSocket handles the second phase without re-sending
- `Transport` enum on `TvConnector`: Network, Serial, Usb, Cec. Daemon scopes NM events to network-dependent backends only
- NetworkManager watcher monitors `StateChanged` on the system bus; `reconnecting` flag prevents NM from racing with in-flight WoL retry loops

## Quick Reference

### Running the Stack

```bash
# Development (session bus):
tisvlayd --session
tisvlay-session --user
tisvlay-tray --user

# Production (system bus, as systemd services):
sudo tisvlayd
systemctl --user enable --now tisvlay-session
systemctl --user enable --now tisvlay-tray
```

### Testing with busctl

```bash
# Manager interface
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 GetSupportedVendors
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig
busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ListSystemProfiles

# List all objects + properties (single round-trip for all TV state)
busctl --user call org.tisvlay.Daemon /org/tisvlay org.freedesktop.DBus.ObjectManager GetManagedObjects

# Per-TV properties (replace living_room with TV ID)
busctl --user introspect org.tisvlay.Daemon /org/tisvlay/Tv/living_room
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1 Connected
busctl --user get-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume Volume

# Writable properties (set via Properties.Set)
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume Volume y 30
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Volume Muted b true
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Screen IsScreenOn b false
busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Backlight Backlight y 80

# Methods (complex operations that aren't simple property sets)
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Power PowerOff
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Power PowerOn
busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/living_room org.tisvlay.Tv1.Input SwitchInput s HDMI_2

# Monitor PropertiesChanged signals
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
