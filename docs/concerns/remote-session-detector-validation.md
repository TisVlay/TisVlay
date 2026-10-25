# Stream detector field validation (deferred)

Field validation of the stream detectors that need more bench setup than a Sunshine/Moonlight session. Tabled after the Sunshine path was field-validated (2026-07-12): the hook/manual path exercised both gate paths (session unlock + world DPMS-on suppression), ordering, latency, and B3's matching-gated dispatch. The detectors below are code-complete and unit/integration-tested, but have not been exercised against their real signal sources on hardware.

## Not yet field-validated (needs bench setup)

- **Portal monitor (krdp).** Needs an RDP client + krdp server configured on the box. Validates: `CreateSession` arming edge, exe attribution (`krdpserver`), the provisional bootstrap join, and the tighter portal-`CreateSession`-vs-injected-input ordering race (the case most likely to need retroactive correction, if any detector does).
- **Portal monitor (RustDesk).** Needs a RustDesk host + a second machine to connect from. Validates portal attribution (`rustdesk`) over the ScreenCast+RemoteDesktop path, and the concurrent-session refcount fix (two clients → the tracker must not disarm on the first disconnect).
- **PipeWire screencast detector.** Exercisable with OBS or a browser tab-share (no remote peer needed). Validates the `Stream/Output/Video` / `kwin_wayland` node predicate and the `screencast_arms` policy (off → no arm; on → arms). Also the InputCapture false-positive exclusion (lan-mouse holds a standing session on this box).
- **logind detector.** Needs an xrdp-class graphical remote login. Validates the graphical-`Type` filter (the SSH `Type=tty` exclusion is proven; a real `x11`/`wayland` `Remote=yes` login is not).

## Phase 0 decisions this validation would settle

These three profile knobs are deliberately **not** in the schema until the bench proves them needed; tabling this validation keeps them deferred (the safe default):

- **Debounce / delay fields** — portal/screencast edge flapping during stream negotiation and client reconnects (scenario C). The Sunshine/hook path shows minimal flapping (the hook brackets app lifetime, not client connects); the portal path is unmeasured.
- **Retroactive correction** — whether the portal arming edge reliably beats injected input; the Sunshine hook wins by design, the portal case is tighter.
- **Sunshine `/serverinfo` poller** — a zero-config fallback for hook-less users; a design call, not blocked on this validation.

## When picked up

Run the scenario matrix below per app (krdp via an RDP client, RustDesk from a second machine), ≥3 cycles each, correlating timestamped traces across terminals by wall-clock:

- **A locked**: screen locked → connect → unlock via remote → interact → disconnect. Race: portal signal vs the ScreenSaver unlock.
- **B dark**: idle until DPMS-off → connect → interact → disconnect. Race: portal/node appearance vs the DPMS-on sysfs flip.
- **C blip**: mid-session client reconnect (RustDesk reconnect, RDP re-attach). Flap: count the portal/node edges per blip to decide whether the debounce delay fields are merited.
- False-positive probes (once): OBS record start/stop, browser tab-share start/stop, Plasma thumbnail hover, Overview toggle — none should arm with "Screen capture arms" off.

Capture terminals (packaged stack running; remote clients on a second device):

```sh
# portal D-Bus signals
dbus-monitor --session "path_namespace='/org/freedesktop/portal/desktop'"
# PipeWire graph events (ts via moreutils: nix shell nixpkgs#moreutils)
stdbuf -oL pw-mon | ts '%H:%M:%.S'
# tisvlay session + daemon logs
journalctl -f -o short-precise --user-unit tisvlay-session
journalctl -f -o short-precise -u tisvlay-daemon
# DPMS sysfs edges (set $p to the TV's connector)
p=/sys/class/drm/cardX-HDMI-A-Y/dpms; l=; while :; do v=$(cat $p); [ "$v" != "$l" ] && echo "$(date +%T.%3N) $v"; l=$v; sleep 0.1; done
```

Fill three decision rows from the traces: race-window sizes (earliest remote signal → unlock/DPMS-on deltas), flap counts per blip, and — for a udev-based live detector — input-device add/remove timing (`udevadm monitor --udev --subsystem-match=input`). The `while-streaming gate suppressed dispatch` daemon log line and the `journalctl -o verbose` `F_EVENT`/`F_TARGET_ON` fields are the load-bearing observations (the Sunshine runs established the reading discipline).
