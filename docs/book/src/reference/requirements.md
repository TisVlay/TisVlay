# Requirements & Compatibility

## Supported TVs

| Backend | Status |
|---------|--------|
| LG webOS (network) | Supported |
| HDMI-CEC | Supported — power + screen-blank on any CEC-capable TV; needs a CEC adapter ([Installation](../getting-started/installation.md#hdmi-cec-backend-optional)) |
| Samsung, Sony, … | Not yet — the architecture is vendor- and transport-agnostic and open to new backends |

LG control works over IPv4 or IPv6; its SSDP discovery and Wake-on-LAN are IPv4-only (the TV doesn't answer v6 multicast, and WoL is an L2 IPv4 broadcast).

## Host requirements

- **Linux** — the services talk to each other over local sockets, and use D-Bus to integrate with the system and desktop. The daemon watches logind on the system bus for sleep, shutdown and the active seat. The **session and tray** services and the app use logind (lock state and remote sessions), GNOME's idle monitor where present, the desktop portals (screen-capture detection, and the app's accent color), the system tray and notifications, so they expect a normal desktop bus.
- **Socket access** — automatic for the user at the console; everyone else needs the `tisvlay` group (the app offers an **Enable TV control** button if you need it). See [Access](../getting-started/installation.md#access).
- **Network reachability** (network backends) — the host can reach the TV; waking it from standby also needs the same L2 network or a relay. See [Troubleshooting → The TV won't power on](../operations/troubleshooting.md#the-tv-wont-power-on).
- **Idle & lock-wake detection** (optional; powers the in-session idle timer and unblanking the TV when you touch the lock screen) — a Wayland compositor with `ext-idle-notify-v1` (KDE Plasma, wlroots compositors) **or GNOME** (detected through Mutter's own idle monitor).
- For the DPMS display-blank automation, the host must expose the matched connector's power state via `/sys/class/drm/*/dpms` (verified on KDE Plasma Wayland).

## What runs where

- `tisvlay-daemon` runs as a dedicated `tisvlay` system user; it owns the control socket (`/run/tisvlay/zenoh.sock` when packaged) and keeps its state under `/var/lib/tisvlay/`.
- `tisvlay-session` and `tisvlay-tray` run as per-user services and reach the daemon over that socket.
- The `tisvlay` CLI targets the same socket; see the [Command Line](../guide/cli.md) page for overriding it.

## Limitations

- Full-feature control (input, volume, backlight) is LG-only for now.
- Over HDMI-CEC only power and screen-blank are supported — volume, backlight, and arbitrary input switching aren't reliable over CEC, and screen-blank is emulated by powering the TV off/on (CEC can't blank a panel that stays on).
- Wake-on-LAN doesn't cross subnets without a relay ([TVs on an Isolated Network](../operations/isolated-networks.md)); CEC TVs wake over the bus instead.
