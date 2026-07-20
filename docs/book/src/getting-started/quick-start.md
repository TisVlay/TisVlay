# Quick Start

This walks through pairing your first TV and controlling it from the command line. It assumes `tisvlay-daemon` is installed and running — automatic after a package install — and that you're at the machine (console access needs no setup; for SSH or scripts, see [Installation](./installation.md#installation) for the one-click `tisvlay enable-access` grant).

## 1. Pair the TV

Make sure the TV is on and on the same network, then pair it. The TV shows an on-screen prompt — accept it.

```sh
tisvlay pair "Living Room" --vendor lg --params '{"ip":"192.168.1.100","mac":"AA:BB:CC:DD:EE:FF"}'
```

- The first argument (`"Living Room"`) is the TV's name.
- `--vendor lg` selects the LG WebOS backend.
- `--params` carries the vendor's pairing fields as JSON. For LG that's `ip` (required) and `mac` (the MAC address for Wake-on-LAN — without it, tisvlay can't power the TV back on from standby).

See [Pairing](./pairing.md) for discovery, the GUI wizard, and HDMI-CEC.

## 2. Control it

Control verbs take the TV's **UUID**. List your TVs to get it:

```sh
tisvlay list
```

Then, using that id (shown as `<id>` below):

```sh
tisvlay power <id> on          # Wake-on-LAN
tisvlay screen <id> off        # OLED-safe blank (TV stays powered)
tisvlay input <id> HDMI_1      # switch input
tisvlay set-volume <id> 25     # set volume (0–100)
tisvlay mute <id> true         # mute (false to unmute)
```

`tisvlay list` shows the current state of every TV; `tisvlay status` is a quick liveliness check on the daemon itself. To watch state change live, use `tisvlay watch`.

## 3. Automate it (optional)

The `tisvlay-session` service can drive the TV from desktop events — for example blanking the screen when you lock, and unblanking when you return. This is configured with **profiles**; see [Session Automation](../guide/session-automation.md) and [Configuration](../guide/configuration.md).

For machine-readable output (scripting, Nushell, etc.), add `--json` to read verbs like `list`.
