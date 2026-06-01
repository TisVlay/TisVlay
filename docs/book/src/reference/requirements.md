# Requirements & Compatibility

## Supported TVs

| Vendor | Status |
|--------|--------|
| LG WebOS | Supported (tested on OLED C3 and G4) |
| Samsung, Sony, … | Not yet — the architecture is vendor-agnostic and open to new backends |

For LG, enable **"TV On via Wi-Fi"** (sometimes "Mobile TV On") in the TV's network settings so Wake-on-LAN works.

## Host requirements

- **Linux with D-Bus** — required for `tisvlayd` and the session/tray services.
- **Network reachability** — the host and TV on the same L2 network (Wake-on-LAN magic packets don't cross routers).
- **Wayland compositor with `ext-idle-notify-v1`** — optional, needed only for greeter-wake detection (unblanking the TV when you interact with the lock screen before typing your password).
- For the DPMS display-blank automation, the host must expose the matched connector's power state via `/sys/class/drm/*/dpms` (verified on KDE Plasma Wayland).

## What runs where

- `tisvlayd` runs on the system bus as a dedicated `tisvlay` user, with state under `/var/lib/tisvlay/`.
- `tisvlay-session` and `tisvlay-tray` run as per-user services on the session bus.
- The `tisvlay` CLI talks to whichever bus the daemon is on (add `--user` for a session-bus daemon started with `tisvlayd --session`).

## Limitations

- LG only, for now.
- Network control only — there is no HDMI-CEC path (by design; the target use case is GPUs without CEC).
- Wake-on-LAN cannot cross subnets, and TVs may briefly ignore it during a pixel-refresh cycle.
