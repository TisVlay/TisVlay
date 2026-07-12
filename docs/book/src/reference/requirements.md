# Requirements & Compatibility

## Supported TVs

| Backend | Status |
|---------|--------|
| LG WebOS (network) | Supported (tested on OLED C3 and G4) |
| HDMI-CEC | Supported — power + screen-blank on any CEC-capable TV; needs a CEC adapter (see below) |
| Samsung, Sony, … | Not yet — the architecture is vendor- and transport-agnostic and open to new backends |

For LG, enable **"TV On via Wi-Fi"** (sometimes "Mobile TV On") in the TV's network settings so Wake-on-LAN works. LG control works over IPv4 or IPv6; SSDP discovery and Wake-on-LAN are IPv4-only (the TV doesn't answer v6 multicast, and WoL is an L2 IPv4 broadcast).

For HDMI-CEC, the kernel must expose a `/dev/cecN` node — most GPU HDMI ports don't wire the CEC pin. Known-working adapters: a **USB CEC adapter** (e.g. Pulse-Eight), or an active **DisplayPort→HDMI adapter on an amdgpu/i915 output** (the GPU driver tunnels DP-AUX CEC — **Nvidia does not**). On NixOS, grant the daemon device access with `services.tisvlay.cec.enable = true`. See [Installation](../getting-started/installation.md#hdmi-cec-backend-optional).

## Host requirements

- **Linux** — the daemon needs no D-Bus (it uses a local IPC socket). The desktop **session and tray** services do use the session/system D-Bus for desktop integration (logind, the screensaver, the system tray, and notifications), so they expect a normal desktop bus.
- **The `tisvlay` group** — a user must be a member to reach the daemon's control socket (the socket is `0660`, group-owned). See [Installation](../getting-started/installation.md).
- **Network reachability** — the host and TV on the same L2 network (Wake-on-LAN magic packets don't cross routers).
- **Wayland compositor with `ext-idle-notify-v1`** — optional, needed only for lock-wake detection (unblanking the TV when you interact with the lock screen before typing your password) and the in-session idle timer.
- For the DPMS display-blank automation, the host must expose the matched connector's power state via `/sys/class/drm/*/dpms` (verified on KDE Plasma Wayland).
- **A CEC adapter** — only for the HDMI-CEC backend; see the adapter note under Supported TVs above.

## What runs where

- `tisvlay-daemon` runs as a dedicated `tisvlay` system user; it owns the control socket (`/run/tisvlay/zenoh.sock` when packaged) and keeps its state under `/var/lib/tisvlay/`.
- `tisvlay-session` and `tisvlay-tray` run as per-user services and reach the daemon over that socket (membership in the `tisvlay` group is what grants them access).
- The `tisvlay` CLI targets the same socket; override it with `--socket` (default `$XDG_RUNTIME_DIR/tisvlay/zenoh.sock`, falling back to the packaged `/run/tisvlay/zenoh.sock`), and `--user-socket` for the session-service verbs (`user-profile`, `remote-session`).

## Limitations

- Full-feature control (input, volume, backlight) is LG-only for now; other brands are reachable over HDMI-CEC for power and screen-blank.
- Over HDMI-CEC only power and screen-blank are supported — volume, backlight, and arbitrary input switching aren't reliable over CEC, and screen-blank is emulated by powering the TV off/on (CEC can't blank a panel that stays on).
- Wake-on-LAN cannot cross subnets, and TVs may briefly ignore it during a pixel-refresh cycle. (The CEC backend wakes over the bus instead, so it doesn't use Wake-on-LAN.)
