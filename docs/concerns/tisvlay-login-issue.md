# Session that boots already-locked never triggers the idle screen-off

## Summary

TisVlay reacts to session **lock/unlock transitions** to drive the TV, but it
does not act on a session that is **already locked when the daemon/session
monitor starts**. On any setup that locks the session at startup (KDE
`LockOnStart=true`, greeter-to-locked handoff, autologin-then-lock, etc.), the
initial lock happens *before* TisVlay is running, so there is no lock edge to
observe. The result is that idling on the boot lock screen never fires the
configured locked idle timeout, and the TV stays on until some *other* event
(e.g. the compositor eventually dropping the connector to DPMS off) happens to
turn it off on a much longer schedule.

A manual unlock→lock cycle produces real edges and works perfectly, which
isolates the gap to the startup-in-locked-state case.

## Environment

- **TisVlay:** 0.3.0
- **OS:** NixOS (flake `services.tisvlay` module), `cec.enable = true`,
  `cec.pulse8.enable = true`
- **Desktop:** KDE Plasma 6 on Wayland (KWin), `ext-idle-notify` v2 server
- **TV:** LG OLED42C3PUA — reachable both over the network (`<lg>` / `wss`) and
  via HDMI-CEC (`<cec>`, `/dev/cec0`), matched to DRM connector
  `card1-HDMI-A-1`
- **Lock config** (`~/.config/kscreenlockerrc`):
  ```ini
  [Daemon]
  Autolock=false
  LockOnStart=true
  Timeout=0
  ```
- **Idle config** (`~/.config/tisvlay/session.toml`):
  ```toml
  [profile.body]
  name = "default"
  locked_idle_timeout_seconds = 5
  unlocked_idle_timeout_seconds = 300
  ```
  Event store also shows `greeter/timeout {"seconds":5}` and
  `profile/system/…5159 {"idle_timeout_seconds":5}`.

## Expected behavior

After booting into a locked session and idling past
`locked_idle_timeout_seconds` (5s), TisVlay turns the matched TV's screen off —
the same action it correctly performs after a live lock transition.

## Actual behavior

Nothing fires from the idle timeout while sitting on the boot lock screen. The
TV only turns off much later, when the HDMI connector independently transitions
to `dpms=Off` and TisVlay mirrors that to the TV (`activestandby`,
`is_screen_on:false`) — i.e. on the compositor's schedule, not the 5s idle
timeout.

## Root cause (inferred, confirmed by the isolation test below)

TisVlay's lock/idle handling is **edge-triggered**. At boot the session comes up
already locked (`LockOnStart=true`, with `Autolock=false` so there is no later
idle→lock edge either), and that initial lock precedes both `tisvlay-daemon` and
`tisvlay-session`. With no lock edge observed after startup, TisVlay appears to
treat the session as unlocked and stays on the unlocked profile, so the 5s
locked-idle path is never armed. It also never records an idle event while the
KDE lock screen is up.

## Evidence

Boot timeline (system booted 10:48:53):

```
# tisvlay-daemon.service
10:49:01  Started tisvlay TV control daemon.
10:49:02  logind watcher started
10:49:10  Registered with TV, got client key
10:49:10  DPMS watcher started
10:49:10  TV matched to DRM connector
# ...no lock/idle/power events logged for the rest of the boot

# tisvlay-session.service (user)
10:49:06  Wayland idle notifier available (server: v2, using: v1)
10:49:06  Wayland idle watch started
# ...silent afterward — no idle event ever logged
```

logind reports the session locked but never idle, the entire time:

```
$ loginctl show-session 1 -p LockedHint -p IdleHint -p IdleSinceHint
LockedHint=yes
IdleHint=no
IdleSinceHint=0
```

The TV state in TisVlay's own event store (`tisvlay watch`) only ever changed
via the DPMS mirror, never via a 5s idle action:

```
tv/<lg>/meta/identity  {… "connector":"card1-HDMI-A-1" … "vendor":"lg"}
tv/<lg>/state/power    {"is_on":true,"is_screen_on":false,"state":"activestandby"}
```

Note: nothing about this appears in the systemd journal at `RUST_LOG=info` — the
state transitions live only in the zenoh event store, which made the silence
easy to misread at first.

## Isolation test (proves the transition path is healthy)

Generating a real lock edge works exactly as designed — the TV turns **on** on
unlock and **off** on lock:

```
loginctl unlock-session 1 && loginctl lock-session 1
```

So the profile logic, DRM-connector match, and TV control paths are all fine.
The only gap is that a session which is *already locked at TisVlay startup* is
never treated as locked.

## Proposed fix

On `tisvlay-session`/daemon startup, **read the initial lock state** (logind
`LockedHint` on the session, or the equivalent for the greeter/session context)
and treat "already locked at start" as a synthetic lock event, arming the locked
profile immediately — rather than relying solely on lock/unlock transitions.
Booting straight into a locked session (`LockOnStart=true`) is a very common
KDE/greeter configuration, so this case is worth handling explicitly.

## Workarounds (config-side, no code change)

- Set `Autolock=true` (with a short idle) in `kscreenlockerrc` so a genuine
  idle→lock **transition** occurs after login, or
- Emit `loginctl lock-session` from a user-session oneshot shortly after login
  to synthesize the edge.

## Possibly related / separate

The **managed CEC device** on `/dev/cec0` fails its power-on handshake
(`Backend error (cec): TV did not report power-on after repeated Image View
On`), while the LG network backend works. Likely a distinct issue, noted here
only for context.
