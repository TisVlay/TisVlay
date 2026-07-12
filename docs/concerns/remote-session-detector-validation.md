# Remote-session detector field validation (deferred)

Field validation of the remote-session detectors that need more bench setup than a Sunshine/Moonlight session. Tabled after the Sunshine path was field-validated (2026-07-12): the hook/manual path exercised both gate paths (session unlock + world DPMS-on suppression), ordering, latency, and B3's matching-gated dispatch. The detectors below are code-complete and unit/integration-tested, but have not been exercised against their real signal sources on hardware.

## Not yet field-validated (needs bench setup)

- **Portal monitor (krdp).** Needs an RDP client + krdp server configured on the box. Validates: `CreateSession` arming edge, exe attribution (`krdpserver`), the provisional bootstrap join, and the tighter portal-`CreateSession`-vs-injected-input ordering race (the case most likely to need retroactive correction, if any detector does).
- **Portal monitor (RustDesk).** Needs a RustDesk host + a second machine to connect from. Validates portal attribution (`rustdesk`) over the ScreenCast+RemoteDesktop path, and the concurrent-session refcount fix (two clients → the tracker must not disarm on the first disconnect).
- **PipeWire screencast detector.** Exercisable with OBS or a browser tab-share (no remote peer needed). Validates the `Stream/Output/Video` / `kwin_wayland` node predicate and the `remote_screencast_arms` policy (off → no arm; on → arms). Also the InputCapture false-positive exclusion (lan-mouse holds a standing session on this box).
- **logind detector.** Needs an xrdp-class graphical remote login. Validates the graphical-`Type` filter (the SSH `Type=tty` exclusion is proven; a real `x11`/`wayland` `Remote=yes` login is not).

## Phase 0 decisions this validation would settle

These three profile knobs are deliberately **not** in the schema until the bench proves them needed; tabling this validation keeps them deferred (the safe default):

- **Debounce / delay fields** — portal/screencast edge flapping during stream negotiation and client reconnects (scenario C). The Sunshine/hook path shows minimal flapping (the hook brackets app lifetime, not client connects); the portal path is unmeasured.
- **Retroactive correction** — whether the portal arming edge reliably beats injected input; the Sunshine hook wins by design, the portal case is tighter.
- **Sunshine `/serverinfo` poller** — a zero-config fallback for hook-less users; a design call, not blocked on this validation.

## When picked up

Run the plan's scenario matrix (A locked / B dark / C blip) per app, capturing timestamped traces (`busctl --user monitor` on the portal namespace, `pw-mon`, the daemon/session journals, dpms sysfs). The `while-remote gate suppressed dispatch` daemon log line and the `journalctl -o verbose` `F_EVENT`/`F_TARGET_ON` fields are the load-bearing observations (the Sunshine runs established the reading discipline).
