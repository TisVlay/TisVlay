<!-- ANCHOR: overview -->
![Banner](docs/artwork/banner.png)

**TisVlay** brings TV Display control for Linux, including: power, volume, backlight, AV mute, input switching plus more, with per-device profile automation via desktop session integration (idle/lock, boot/suspend, remote login/streaming, etc), while supporting various vendor interface protocols for heterogeneous multi-display setups, mitigating energy usage and display burn-in.

[![CI Pipeline](https://github.com/TisVlay/TisVlay/actions/workflows/ci.yml/badge.svg)](https://github.com/TisVlay/TisVlay/actions/workflows/ci.yml)
[![Crates.io Version](https://img.shields.io/crates/v/tisvlay)](https://crates.io/crates/tisvlay)
[![Crates.io Dependencies](https://img.shields.io/deps-rs/tisvlay/latest)](https://crates.io/crates/tisvlay/dependencies)
![Crates.io MSRV](https://img.shields.io/crates/msrv/tisvlay)
![Crates.io Total Downloads](https://img.shields.io/crates/d/tisvlay)
[![Crates.io License](https://img.shields.io/crates/l/tisvlay)](https://choosealicense.com/licenses/apache-2.0)

## ✨ Features
- 📺 Control multiple TVs & projectors just like conventional displays
  - Power on/off with PC, Screen on/off with lock screen
  - Easily adjust backlight, volume, active input, and more
- 🤖 Event driven automation driven via system and user events
  - System boot, reboot, shutdown, suspend, wake, DPMS
  - User lock/unlock, idle, remote session connect/disconnect
- 🧰 Flexible profile customization and switching
  - System-wide and per-user profiles for per-device behavior
  - Unified API and CLI for further scripting and integrations
- 🧩 Vendor agnostic & modular architecture
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

<!-- ANCHOR_END: overview -->

## 📖 Documentation

The user guide is an [mdBook](https://rust-lang.github.io/mdBook/) in [`docs/book/`](docs/book/) (`just docs-serve` renders it locally):

- 🧭 New here? Start with [Installation](docs/book/src/getting-started/installation.md), then [Quick Start](docs/book/src/getting-started/quick-start.md).
- 🎛️ Already running? See the [CLI Reference](docs/book/src/guide/cli.md) and [Configuration](docs/book/src/guide/configuration.md).
- 🛰️ Streaming in remotely? See [Remote-Session Awareness](docs/book/src/guide/remote-sessions.md).
- 🔎 Discovery not finding your TV? See [Firewall & Discovery](docs/book/src/operations/firewall-discovery.md).
- 🔒 TV discovering too much about you? See [TVs on an Isolated Network](docs/book/src/operations/isolated-networks.md).
