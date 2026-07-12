# Remote-Session Awareness

When you remote into this machine — Sunshine/Moonlight game streaming, RustDesk, or krdp/RDP — the remote session unlocks the desktop and feeds it virtual keyboard and mouse input. To your desktop that looks exactly like someone sitting down: the idle timer resets, the outputs un-blank, and tisvlay's normal automation would **power the physical TV on for an empty room**. Remote-session awareness stops that.

While a remote session is active, tisvlay marks the affected TVs as **armed** and suppresses the wake actions, while still letting blanks and power-offs through. When you physically walk back to the TV, it wakes normally through the usual unlock/idle path.

The feature is **inert on a stock desktop** — nothing arms until a detector or hook fires — so it costs you nothing until you actually stream in.

## What it does when armed

- **Wake events are suppressed** — `on_session_unlock`, `on_lock_wake`, `on_idle_resume`, and the daemon's `on_dpms_on`/`on_wake` don't fire while the TV is armed. This is the suppress-by-default rule: a plain action is quiet while armed.
- **Off-going events still fire** — blanks and power-offs (`on_session_lock`, idle blank, `on_sleep`, `on_dpms_off`, …) are exempt, so a TV that's on still darkens for the empty room.
- **The arm and disarm edges act** — `on_remote_connect` fires when the session arms the TV (default `screen_off`, so the panel blanks) and `on_remote_disconnect` when it ends (also `screen_off` — the room is still empty; you wake it when you return).

Every one of these is a per-event choice: each action row carries a **while-remote** toggle (in the GUI, or `while_remote = true` on the action in TOML) that opts the event back into firing while armed. See [Configuration](./configuration.md#action-vocabulary) and [Profiles](./profiles.md#the-remote-session-gate).

## What tisvlay recognizes

| Source | Arms | Notes |
|--------|------|-------|
| Sunshine / Moonlight | automatically | zero-config (a loopback status poll); no hook required |
| RustDesk, krdp/RDP | automatically | detected through the desktop's screen-capture portal |
| Any screen capture (OBS, browser tab-share, …) | only if **Screen capture arms** is on | it can't tell *who* is capturing, so it's opt-in |
| Manual / your own script | when you call the CLI verb | the escape hatch for anything else |

A software-KVM like **lan-mouse** (which holds a standing input-capture session while you're physically present) never arms. Plain SSH sessions don't arm either — only a graphical remote *desktop* does.

The **Screen capture arms** switch lives on the user profile's REMOTE SESSION card in the GUI (or `remote_screencast_arms = true` in the profile). Turn it on if you want any screen recording to suppress TV wakes; leave it off (the default) and only the recognized remote apps arm.

## Sunshine integration

Sunshine's default capture on Nvidia is invisible to the desktop portal, so tisvlay watches Sunshine's own status endpoint on loopback instead — which means Sunshine works **with no configuration at all**. Just stream in; the TV stays asleep.

If you want the TV armed the *instant* Sunshine launches (a second or two ahead of the status poll), add a prep-command hook to Sunshine (`sunshine.conf` → **Configuration → General → Command Preparations**, or the raw config):

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

The hook is optional and purely a latency optimization — the poll covers the disarm and works on its own. If you run an unpackaged build (a dev checkout, a manually installed CLI), give the hook the absolute path to `tisvlay` and pass the `--socket`/`--user-socket` flags explicitly, since Sunshine runs hooks outside your shell environment.

## Manual control

For anything tisvlay doesn't recognize, arm and disarm it yourself:

```sh
tisvlay remote-session start --source myapp   # arm
tisvlay remote-session stop  --source myapp   # disarm
tisvlay remote-session status                 # show live sources + armed TVs
```

These run against the session service, so include `--user-socket` if you've moved it from the default.

## Seeing it work

- The tray shows a read-only **Remote session active** indicator while any TV is armed.
- `tisvlay remote-session status` prints the current sources and which TVs are armed.
- `tisvlay --user-socket <sock> watch` streams the remote-session fact live alongside everything else.
