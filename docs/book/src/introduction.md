# tisvlay

**TV display control for Linux.**

tisvlay controls smart TVs from a Linux desktop — power, screen blanking, HDMI input, volume, and backlight — and ties that control to your desktop session (lock/unlock, suspend/resume, idle, and display blanking). It reaches a TV two ways: over the **network** (LG WebOS — no extra hardware, and it works even where the GPU exposes no HDMI-CEC, as on Nvidia) or over **HDMI-CEC** (the Linux CEC subsystem, through a USB CEC adapter or an amdgpu/i915 DisplayPort→HDMI adapter).

Today tisvlay supports **LG WebOS** TVs over the network, plus any TV that speaks **HDMI-CEC** (power and screen-blanking) through a CEC adapter. The architecture is vendor- and transport-agnostic, so other backends (Samsung, Sony, …) can be added later.

## Features

- **Power** — turn the TV on (Wake-on-LAN, or a CEC bus wake) and off (standby).
- **Screen blanking** — OLED-safe screen on/off for burn-in protection, distinct from a full power-off.
- **HDMI input switching** — change inputs programmatically.
- **Volume & backlight** — adjust volume, mute, and OLED pixel brightness.
- **Desktop automation** — react to session lock/unlock, suspend/resume, idle timeout, and host-display DPMS blank/wake.
- **Multi-TV** — manage several TVs, each matched to its host display connector by EDID.

## Components

tisvlay is a small set of cooperating programs:

| Component | What it is |
|-----------|------------|
| `tisvlay` | Command-line tool for direct control and configuration |
| `tisvlayd` | System daemon that owns the TV connections and exposes a D-Bus interface |
| `tisvlay-session` | Per-user service that maps desktop events (lock, idle, …) to TV actions |
| `tisvlay-tray` | System-tray applet for quick control and status |
| `tisvlay-gui` | Settings window (pair TVs, edit profiles) |

The CLI and the desktop services all talk to `tisvlayd` over D-Bus; the daemon holds the persistent configuration and the live TV connections.

```mermaid
graph LR
    subgraph User session
        Session["tisvlay-session<br/><i>desktop events</i>"]
        Tray["tisvlay-tray<br/><i>system tray</i>"]
        Gui["tisvlay-gui<br/><i>settings</i>"]
        Cli["tisvlay<br/><i>CLI</i>"]
    end

    subgraph System
        Daemon["tisvlayd<br/><i>system daemon</i>"]
    end

    subgraph TVs
        TV1["LG TV"]
        TV2["CEC TV"]
    end

    Session -->|D-Bus| Daemon
    Tray -->|D-Bus| Daemon
    Gui -->|D-Bus| Daemon
    Cli -->|D-Bus| Daemon
    Daemon -.->|WebSocket / WoL| TV1
    Daemon -.->|HDMI-CEC| TV2
```

## Where to go next

- New here? Start with [Installation](./getting-started/installation.md), then [Quick Start](./getting-started/quick-start.md).
- Already running? See the [CLI Reference](./guide/cli.md) and [Configuration](./guide/configuration.md).
- Discovery not finding your TV? See [Firewall & Discovery](./operations/firewall-discovery.md).
