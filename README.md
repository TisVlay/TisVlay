![Banner](docs/artwork/banner.png)

# tisvlay

TV Display control for Linux

## Features

- **Power Management**: Turn TV on (Wake-on-LAN) and off
- **Screen Blanking**: OLED-safe screen on/off for burn-in protection
- **HDMI Input Switching**: Switch inputs programmatically
- **Volume Control**: Adjust volume, mute/unmute
- **Desktop Integration**: Automatic screen control on lock/unlock, suspend/resume
- **Wayland Support**: Detects lock screen interaction to unblank TV before password entry
- **Mock Backend**: a built-in "Mock (testing)" vendor — add a mock TV to verify the daemon / desktop stack without real hardware

## Components

| Component | Description |
|-----------|-------------|
| `tisvlay` | CLI tool for direct TV control |
| `tisvlay-daemon` | System daemon: bevy_ecs world + zenoh wire host |
| `tisvlay-session` | User service for desktop event integration and user profiles |
| `tisvlay-tray` | System tray with live TV controls and desktop notifications |
| `tisvlay-gui` | Settings and management window (Slint) |
| `tisvlay-greeter` | Login-greeter idle client (OLED protection at the login screen) |

The processes talk over [zenoh](https://zenoh.io/) on Unix sockets: the daemon owns the system socket (`/run/tisvlay/zenoh.sock` packaged, `$XDG_RUNTIME_DIR/tisvlay/zenoh.sock` in dev), and the session service owns a per-user socket for user profiles. Socket permission is the access model — accounts in the `tisvlay` group can control the daemon.

## Quick Start

```bash
# Build all components (the shipping binaries; add --workspace for the
# libraries + dev/test crates, or use `just check`)
cargo build --release

# Pair with your TV (shows prompt on TV screen)
tisvlay pair "Living Room" --vendor lg --json '{"ip":"192.168.1.100"}'

# Control the TV (UUIDs from `tisvlay list`)
tisvlay list
tisvlay power-on <uuid>
tisvlay set-screen <uuid> false
tisvlay switch-input <uuid> HDMI_1
tisvlay set-volume <uuid> 25
```

## Profiles

Desktop events (lock/unlock, sleep/wake, idle, DPMS) map to configurable per-TV actions through system and user profiles. Both have an editable `default` profile that TVs inherit from; manage them with `tisvlay profile …` / `tisvlay user-profile …`, the tray's Profiles submenu, or the GUI's Profiles pane.

## Documentation

- [.claude/CLAUDE.md](.claude/CLAUDE.md) - Detailed architecture and implementation notes
- [docs/architecture.md](docs/architecture.md) - Current architecture: key space, topology, world/reactor model
- [docs/firewall.md](docs/firewall.md) - SSDP discovery firewall configuration

## Requirements

- LG WebOS TV (tested with C3, G4 models) or an HDMI-CEC adapter exposing `/dev/cecN`
- "TV On via Wi-Fi" enabled in TV settings (LG network control)
- KDE Plasma or GNOME on Wayland for the desktop session-event automation (lock via logind `LockedHint`; idle via `ext-idle-notify-v1` on KWin/wlroots, or `org.gnome.Mutter.IdleMonitor` on GNOME)

## License

See [LICENSE](LICENSE) for details.
