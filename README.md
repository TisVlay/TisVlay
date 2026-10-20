<!-- ANCHOR: overview -->
![Banner](docs/artwork/banner.png)

**TisVlay** brings TV Display control for Linux, including: power, volume, backlight, AV mute, input switching plus more, with per-device profile automation via desktop session integration (idle/lock, boot/suspend, remote login/streaming, etc), while supporting various vendor interface protocols for heterogeneous multi-display setups, mitigating energy usage and OLED panel burn-in or projector bulb burn-out.

[![CI Pipeline](https://github.com/TisVlay/TisVlay/actions/workflows/ci.yml/badge.svg)](https://github.com/TisVlay/TisVlay/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/TisVlay/TisVlay/branch/main/graph/badge.svg)](https://codecov.io/gh/TisVlay/TisVlay)
[![Latest release](https://img.shields.io/github/v/release/TisVlay/TisVlay)](https://github.com/TisVlay/TisVlay/releases)
[![License](https://img.shields.io/github/license/TisVlay/TisVlay)](https://github.com/TisVlay/TisVlay/blob/main/LICENSE)

## ✨ Features
- 📺 Conventional multi-monitor control
  - Power on/off with PC, Screen on/off with lock screen
  - Easily adjust backlight, volume, active input, and more
- 🤖 Automation via system/user events
  - System boot, reboot, shutdown, suspend, wake, DPMS
  - User lock/unlock, idle, remote session connect/disconnect
- 🧰 Flexible profile customization
  - System-wide and per-user profiles for per-device behavior
  - Unified API and CLI for further scripting and integrations
- 🧩 Modular & vendor agnostic
  - Supports HDMI CEC, Network IP Control (LG WebOS SSAP)
  - Generalized for further interfaces, protocols, and transports
- 🩺 Self-healing connectivity
  - Interface status monitoring for automatic reconnect and retry
  - Per-NIC kernel carrier detection with route-aware failover
- 📡 Input-aware automation gating
  - Suppress automation when TV already in use via another input
  - Prevent PC from interrupting movie sessions or console gaming
- 🖱️ Graphical User Interfaces
  - GUI app for device pairing, profile editing, and user setting
  - System tray for quick access controls and device status

## 📸 Screenshots

| **TVs** — devices, status, profiles (dark theme) | **Controls** — picture, audio, input (light theme) |
|:---:|:---:|
| ![TVs pane showing a connected LG TV with its status, managed toggle and profile assignments](docs/screenshots/tvs_pane.png) | ![TVs pane in the light theme showing backlight, screen, volume, mute and input controls](docs/screenshots/tvs_pane_2.png) |
| **Discovery** — local device detection | **Pairing** — auto populate configuration |
| ![Device discovery pane showing detected TVs on the local network](docs/screenshots/wizard_discovery.png) | ![Device pairing pane with auto populated configuration for a TV](docs/screenshots/wizard_pairing.png) |
| **System profiles** — machine lifecycle events | **User profiles** — session events and idle timers |
| ![Profiles pane editing a system profile's startup, shutdown, sleep, wake and DPMS actions](docs/screenshots/profiles_pane_system.png) | ![Profiles pane editing a user profile's lock, unlock, idle and resume actions](docs/screenshots/profiles_pane_user.png) |
| **Settings** — desktop notification preferences | **About** — components, their status and versions |
| ![Settings pane with notification toggles and a test-notification button](docs/screenshots/settings_pane.png) | ![About pane listing the daemon, session, tray, GUI and CLI with their versions](docs/screenshots/about_pane.png) |
| **Tray menu** — per-TV quick control settings | **Tray tooltip** — at a glance or scroll to adjust |
| ![System tray menu with power, profiles, input, backlight, screen and volume controls for a TV](docs/screenshots/system_tray_menu.png) | ![System tray tooltip listing each TV's power, volume and backlight](docs/screenshots/system_tray_hover.png) |


## 🏛️ Architecture

TisVlay is comprised of a collection of cooperating components:

```mermaid
graph LR

subgraph User
    Gui["tisvlay-gui<br/><i>user settings</i>"]
    Cli["tisvlay<br/><i>CLI</i>"]
    Session["tisvlay-session<br/><i>desktop events</i>"]
    Tray["tisvlay-tray<br/><i>system tray</i>"]
end

subgraph System
    Daemon["tisvlay-daemon<br/><i>system daemon</i>"]
    Greeter["tisvlay-greeter<br/><i>login greeter</i>"]
end

subgraph Devices
    TV1["CEC TV"]
    TV2["LG TV"]
end

Session -->|socket| Daemon
Tray -->|socket| Daemon
Gui -->|socket| Daemon
Cli -->|socket| Daemon
Daemon -.->|HDMI-CEC| TV1
Daemon -.->|WebSocket / WoL| TV2
```

| Component | Description |
|-----------|-------------|
| `tisvlay` | CLI for direct TisVlay control and configuration |
| `tisvlay-daemon` | System profile actor and device interface driver |
| `tisvlay-session` | User profile actor and session event integration |
| `tisvlay-tray` | System tray for quick access and desktop notifications |
| `tisvlay-gui` | Device management, profile editor, settings panel |
| `tisvlay-greeter` |  Login greeter idle detection absent of active session |

Service ship as `tisvlay` CLI subcommands, plus separate `tisvlay-gui` binary.

<!-- ANCHOR_END: overview -->

## 📖 Documentation

- 🧭 New here? Start with [Installation](docs/book/src/getting-started/installation.md), then [Quick Start](docs/book/src/getting-started/quick-start.md).
- 🎛️ Setting things up? See [GUI & Tray](docs/book/src/guide/gui-tray.md) and [Automation](docs/book/src/guide/automation.md).
- 🛰️ Streaming in remotely? See [Remote-Session Awareness](docs/book/src/guide/remote-sessions.md).
- 🔎 Discovery not finding your TV? See [Firewall & Discovery](docs/book/src/operations/firewall-discovery.md).
- 🔒 TV discovering too much about you? See [TVs on an Isolated Network](docs/book/src/operations/isolated-networks.md).
- ⌨️ Scripting? See the [Command Line](docs/book/src/guide/cli.md) and [Configuration Files](docs/book/src/guide/configuration.md).

## 🙏 Acknowledgments

Full third-party notice included with releases. Notable Rust crates include:

- [Slint](https://slint-ui.com) – Declarative GUI toolkit powering `tisvlay-gui`'s responsive interface
- [Bevy](https://bevyengine.org) – ECS used in `tisvlay-daemon` to manage device interactions and events
- [Zenoh](https://www.zenoh.io) – Zero-overhead pub/sub unifying socket IPC across the `tisvlay` stack
- [ksni](https://github.com/iovxw/ksni) – Freedesktop StatusNotifierItem library powering `tisvlay-tray`
- [linux-cec](https://gitlab.steamos.cloud/holo/linux-cec/) – Linux userspace bindings used by `tisvlay-cec` for HDMI-CEC
- [netflector](https://github.com/netflector/netflector) – Reflector enabling mDNS, SSDP, WoL while preserving VLAN privacy
