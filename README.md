# orlgtv

Open Remote for LG TVs on Linux - Control LG WebOS TVs from your Linux desktop.

## Features

- **Power Management**: Turn TV on (Wake-on-LAN) and off
- **Screen Blanking**: OLED-safe screen on/off for burn-in protection
- **HDMI Input Switching**: Switch inputs programmatically
- **Volume Control**: Adjust volume, mute/unmute
- **Desktop Integration**: Automatic screen control on lock/unlock, suspend/resume
- **Wayland Support**: Detects lock screen interaction to unblank TV before password entry

## Components

| Component | Description |
|-----------|-------------|
| `orlgtv` | CLI tool for direct TV control |
| `orlgtvd` | System daemon with D-Bus interface |
| `orlgtv-session` | User service for desktop event integration |

## Quick Start

```bash
# Build all components
cargo build --release

# Pair with your TV (shows prompt on TV screen)
orlgtv pair 192.168.1.100 --name "Living Room" --mac AA:BB:CC:DD:EE:FF

# Control the TV
orlgtv power on
orlgtv screen off
orlgtv input HDMI_1
orlgtv volume 25
```

## Session Service

The session service automatically controls your TV based on desktop events:

```toml
# ~/.config/orlgtv/config.toml
[session]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"
on_greeter_wake = "screen_on"  # Unblank when interacting with lock screen (Wayland only)
on_sleep = "power_off"
on_wake = "power_on"
```

## Documentation

- [CLAUDE.md](CLAUDE.md) - Detailed architecture and implementation notes
- [docs/design.md](docs/design.md) - Full system design
- [docs/firewall.md](docs/firewall.md) - SSDP discovery firewall configuration

## Requirements

- LG WebOS TV (tested with C3, G4 models)
- "TV On via Wi-Fi" enabled in TV settings
- Linux with D-Bus (for daemon/session service)
- Wayland compositor with `ext-idle-notify-v1` support (for greeter wake detection)

## License

See [LICENSE](LICENSE) for details.
