# Quick Start

This walks through pairing your first TV and controlling it from the command line, using an LG webOS TV on your network as the example; for an HDMI-CEC TV, see [Pairing](./pairing.md#hdmi-cec-no-pairing-needed) instead. It assumes `tisvlay-daemon` is installed and running — automatic after a package install — and that you're at the machine (from SSH or scripts, see [Access](./installation.md#access)).

## 1. Pair the TV

Make sure the TV is on and on the same network, then pair it. The TV shows an on-screen prompt — accept it.

```sh
tisvlay pair "Living Room" --vendor lg --params '{"ip":"192.168.1.100"}'
```

- The first argument (`"Living Room"`) is the TV's name.
- `--vendor lg` selects the LG webOS backend.
- `--params` carries the backend's pairing fields as JSON — for LG, the TV's `ip`. tisvlay reads the TV's MAC address (for waking it from standby) when pairing completes; see [Pairing](./pairing.md#the-wake-on-lan-mac).

## 2. Control it

Control verbs take the TV's **UUID**. List your TVs to get it:

```sh
tisvlay list
```

Then, using that id (shown as `<id>` below):

```sh
tisvlay power <id> on          # wake it from standby
tisvlay screen <id> off        # OLED-safe blank (TV stays powered)
tisvlay input <id> HDMI_1      # switch input
tisvlay set-volume <id> 25     # set volume (0–100)
tisvlay mute <id> true         # mute (false to unmute)
```

To watch state change live, use `tisvlay watch`; the [CLI Reference](../guide/cli.md) covers every verb.

## 3. Automate it (optional)

The `tisvlay-session` service can drive the TV from desktop events — for example blanking the screen when you lock, and unblanking when you return. This is configured with **profiles**; see [Session Automation](../guide/session-automation.md).
