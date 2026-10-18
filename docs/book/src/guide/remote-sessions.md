# Remote-Session Awareness

When you connect to this computer from elsewhere (game streaming with Sunshine and Moonlight, RustDesk, or an RDP client), the remote session unlocks the desktop and feeds it virtual keyboard and mouse input. Your desktop treats that exactly like someone sitting down, so without help TisVlay would **power on the physical TV in an empty room**.

Remote-session awareness prevents that. While a remote session is active, TisVlay marks the affected TVs as **armed**: it holds back the actions that would turn them on, while still letting blanks and power-offs through. When you walk back to the machine, the TV wakes on your first touch of the mouse or keyboard.

You don't need to set anything up for the common cases. Just connect.

## What it does when armed

- **Wake actions are held back.** Unlocking, touching the lock screen, activity resuming from idle, and your display waking (DPMS) don't turn the TV on while it's armed.
- **Blanks and power-offs still happen.** Locking, idle blanking, sleep, and display blanking go through, so a TV that's on still darkens for the empty room.
- **Connecting and disconnecting have their own actions.** When a session arms the TV, the profile's **On remote connect** action runs (by default `screen_off`, so the panel blanks). When it ends, **On remote disconnect** runs (also `screen_off`, since the room is still empty).
- **Your return wakes it.** After a session ends, your first genuine touch of the PC's mouse or keyboard runs **On local return** (by default `screen_on`, switching back to your PC's input). This covers the case where your desktop never locked or idled while you were away, so no unlock event would otherwise relight the TV.

You can override the hold-back per event. In the **Profiles** editor, each event row has a network icon: when it shows **Suppressed during remote session**, the event is held back while armed; click it so it shows **Fires during remote session** to let the event run anyway. See [Automation](./automation.md#changing-what-happens).

## What counts as a remote session

| Source | Arms the TV | Notes |
|--------|-------------|-------|
| Sunshine / Moonlight | automatically | no setup; TisVlay detects it |
| RustDesk, RDP | automatically | detected through the desktop's screen-sharing system |
| Any other screen capture (OBS, a browser tab share, …) | only if you turn on **Screen capture arms** | TisVlay can't tell who's capturing, so it's opt-in |
| Your own script or tool | when it tells TisVlay | see [Advanced](#advanced-sunshine-and-manual-control) |

Software KVMs such as lan-mouse never arm, because they run while you're physically present. A plain SSH login doesn't arm either; only a graphical remote desktop does.

**Screen capture arms** is a switch on the **REMOTE SESSION** card of a *user* profile, in the **Profiles** page. Turn it on if you want any screen recording to hold back TV wakes. Leave it off (the default) and only the recognized remote apps arm.

## Seeing it work

While any TV is armed, the tray menu shows a read-only **Remote session active** line. That's the quickest way to tell that TV wakes are being held back on purpose; see [GUI & Tray](./gui-tray.md#tray-applet-tisvlay-tray).

## Advanced: Sunshine and manual control

Everything here is optional. Sunshine works without it, and the command line is only needed to arm TisVlay from your own scripts.

### Sunshine integration

TisVlay detects Sunshine by watching Sunshine's own status endpoint on this computer, which is why no configuration is needed. That endpoint sits on Sunshine's base port, 47989 by default. If you've moved Sunshine to another `port`, tell TisVlay where to look, or it will poll whatever now answers on the default port:

- **NixOS:** `services.tisvlay.sunshinePort = 48989;` (Sunshine's configured `port`).
- **Other distros:** add `--sunshine-port 48989` to the session service's command line with `systemctl --user edit --full tisvlay-session`, then `systemctl --user restart tisvlay-session`.

To arm the TV the instant Sunshine launches, a second or two ahead of the status check, add a prep-command hook to Sunshine's `global_prep_cmd` setting:

```json
{
  "global_prep_cmd": [
    {
      "do": "tisvlay remote-session start --source sunshine",
      "undo": "tisvlay remote-session stop --source sunshine",
      "elevated": false
    }
  ]
}
```

The hook is purely a latency optimization. If you run an unpackaged build, give the hook the absolute path to `tisvlay` and pass the `--socket` and `--user-socket` flags explicitly, since Sunshine runs hooks outside your shell environment.

### Manual control

For a remote tool TisVlay doesn't recognize, arm and disarm it yourself:

```sh
tisvlay remote-session start --source myapp   # arm
tisvlay remote-session stop  --source myapp   # disarm
tisvlay remote-session status                 # show live sources + armed TVs
```

These talk to your user session service, so add `--user-socket` if you've moved it from the default. `tisvlay --user-socket <sock> watch` streams the remote-session state live alongside everything else.
