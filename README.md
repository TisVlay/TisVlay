# tisvlay

TV Display control for Linux

📖 **[User documentation](https://tisvlay.readthedocs.io)** — install, pairing, CLI reference, configuration, and troubleshooting (built from [`docs/book/`](docs/book/)).

## Features

- **Power Management**: Turn TV on (Wake-on-LAN) and off
- **Screen Blanking**: OLED-safe screen on/off for burn-in protection
- **HDMI Input Switching**: Switch inputs programmatically
- **Volume Control**: Adjust volume, mute/unmute
- **Desktop Integration**: Automatic screen control on lock/unlock, suspend/resume
- **Wayland Support**: Detects lock screen interaction to unblank TV before password entry
- **Mock Backend**: a built-in "Mock (testing)" vendor ships in the packages for field debugging — add a mock TV to verify the daemon / D-Bus / desktop stack without real hardware

## Components

| Component | Description |
|-----------|-------------|
| `tisvlay` | CLI tool for direct TV control |
| `tisvlayd` | System daemon with D-Bus interface |
| `tisvlay-session` | User service for desktop event integration |

## Quick Start

```bash
# Build all components (the shipping binaries; add --workspace for the
# libraries + dev/test crates, or use `just check`)
cargo build --release

# Pair with your TV (shows prompt on TV screen)
tisvlay pair 192.168.1.100 --vendor lg --name "Living Room" --wake-id AA:BB:CC:DD:EE:FF

# Control the TV
tisvlay power on
tisvlay screen off
tisvlay input HDMI_1
tisvlay volume 25
```

## Session Service

The session service automatically controls your TV based on desktop events:

```toml
# ~/.config/tisvlay/session.toml
[session]
on_session_lock = "screen_off"
on_session_unlock = "screen_on"
on_lock_wake = "screen_on"  # Unblank when interacting with lock screen (Wayland only)
on_sleep = "power_off"
on_wake = "power_on"
```

## Developer documentation

User-facing docs live at the [user documentation](https://tisvlay.readthedocs.io) site above. For internals:

- [docs/design.md](docs/design.md) - Full system design
- [.claude/CLAUDE.md](.claude/CLAUDE.md) - Detailed architecture and implementation notes
- [docs/firewall.md](docs/firewall.md) - SSDP discovery firewall configuration

## Requirements

- LG WebOS TV (tested with C3, G4 models)
- "TV On via Wi-Fi" enabled in TV settings
- Linux with D-Bus (for daemon/session service)
- Wayland compositor with `ext-idle-notify-v1` support (for lock wake detection)

## License

See [LICENSE](LICENSE) for details.
