# Quick Start

This walks through pairing your first TV and controlling it from the command line. It assumes `tisvlayd` is installed and running (see [Installation](./installation.md)).

## 1. Pair the TV

Make sure the TV is on and on the same network, then pair it. The TV shows an on-screen prompt — accept it.

```sh
tisvlay pair 192.168.1.100 --vendor lg --name "Living Room" --wake-id AA:BB:CC:DD:EE:FF
```

- `--vendor lg` selects the LG WebOS backend.
- `--wake-id` is the TV's MAC address, used for Wake-on-LAN. Without it, tisvlay can't power the TV back on from standby.

See [Pairing](./pairing.md) for discovery and troubleshooting.

## 2. Control it

```sh
tisvlay power on          # Wake-on-LAN
tisvlay screen off        # OLED-safe blank (TV stays powered)
tisvlay input HDMI_1      # switch input
tisvlay volume 25         # set volume (0–100)
tisvlay volume +5         # relative adjustment
tisvlay mute              # toggle mute
tisvlay status            # show current state
```

When you have more than one TV, target a specific one with `--tv <id>` (see `tisvlay list` for the IDs).

## 3. Automate it (optional)

The `tisvlay-session` service can drive the TV from desktop events — for example blanking the screen when you lock, and unblanking when you return. This is configured with **profiles**; see [Session Automation](./session-automation.md) and [Configuration](./guide/configuration.md).

For machine-readable output (scripting, Nushell, etc.), add `--json` to any command.
