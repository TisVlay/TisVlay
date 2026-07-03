# Requirements & Compatibility

## Supported TVs

| Backend | Status |
|---------|--------|
| LG WebOS (network) | Supported (tested on OLED C3 and G4) |
| HDMI-CEC | Supported — power + screen-blank on any CEC-capable TV; needs a CEC adapter (see below) |
| Samsung, Sony, … | Not yet — the architecture is vendor- and transport-agnostic and open to new backends |

For LG, enable **"TV On via Wi-Fi"** (sometimes "Mobile TV On") in the TV's network settings so Wake-on-LAN works.

For HDMI-CEC, the kernel must expose a `/dev/cecN` node — most GPU HDMI ports don't wire the CEC pin. Known-working adapters: a **USB CEC adapter** (e.g. Pulse-Eight), or an active **DisplayPort→HDMI adapter on an amdgpu/i915 output** (the GPU driver tunnels DP-AUX CEC — **Nvidia does not**). On NixOS, grant the daemon device access with `services.tisvlay.cec.enable = true`. See [Installation](../getting-started/installation.md#hdmi-cec-backend-optional).

## Host requirements

- **Linux with D-Bus** — required for `tisvlayd` and the session/tray services.
- **Network reachability** — the host and TV on the same L2 network (Wake-on-LAN magic packets don't cross routers).
- **Wayland compositor with `ext-idle-notify-v1`** — optional, needed only for lock-wake detection (unblanking the TV when you interact with the lock screen before typing your password) and the in-session idle timer.
- For the DPMS display-blank automation, the host must expose the matched connector's power state via `/sys/class/drm/*/dpms` (verified on KDE Plasma Wayland).
- **A CEC adapter** — only for the HDMI-CEC backend; see the adapter note under Supported TVs above.

## What runs where

- `tisvlayd` runs on the system bus as a dedicated `tisvlay` user, with state under `/var/lib/tisvlay/`.
- `tisvlay-session` and `tisvlay-tray` run as per-user services on the session bus.
- The `tisvlay` CLI talks to whichever bus the daemon is on (add `--user` for a session-bus daemon started with `tisvlayd --session`).

## Limitations

- Full-feature control (input, volume, backlight) is LG-only for now; other brands are reachable over HDMI-CEC for power and screen-blank.
- Over HDMI-CEC only power and screen-blank are supported — volume, backlight, and arbitrary input switching aren't reliable over CEC, and screen-blank is emulated by powering the TV off/on (CEC can't blank a panel that stays on).
- Wake-on-LAN cannot cross subnets, and TVs may briefly ignore it during a pixel-refresh cycle. (The CEC backend wakes over the bus instead, so it doesn't use Wake-on-LAN.)
