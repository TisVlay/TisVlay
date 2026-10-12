# Remote-session detection follow-ups

Deferred work parked when the remote-session-awareness feature merged. The feature ships with five detectors (manual/hook, PipeWire, portal, logind, Sunshine poll), the per-event `while_remote` gate, and the `on_remote_connect`/`on_remote_disconnect`/`on_local_return` events. These are the "later, if asked" extensions. See [remote/detection.md](../remote/detection.md) for the as-built reference.

Related deferrals filed separately: the world-side eager remote-state refresh ([eager-remote-state-refresh.md](eager-remote-state-refresh.md)), and the on-hardware detector validation plus the two bench-gated knobs — connect/disconnect debounce **delay fields** and **retroactive correction** — in [concerns/remote-session-detector-validation.md](../concerns/remote-session-detector-validation.md).

## New detectors

- **Steam Remote Play / Parsec.** Both are portal-invisible on Nvidia like Sunshine (their own capture paths). Each would need its own signal probe — a poll endpoint, a hook, or a per-client input-device pattern — evaluated the way Sunshine's `/serverinfo` was. Trigger: a user reports one of them waking the TV.
- **udev per-client input-device watcher (Sunshine live-detection).** Sunshine creates uinput devices per connected client (gamepad/touch/pen); a `udev` monitor on the `input` subsystem could arm on their appearance as a live, event-driven signal — complementing the `/serverinfo` level poll (keyboard/mouse are persistent, so coverage is partial). Confirmed real by vuinputd. Trigger: the poll cadence proves too coarse in practice, or a hookless zero-latency arm is wanted.
- **journald-tail detectors.** Rejected for v1 — matching on an app's English log literals is brittle and breaks silently on a version bump. Revisit only if a target app loses every structured signal and a log line is the sole remaining evidence.

## Portability

- **GNOME / Mutter.** The PipeWire node predicate (KWin's `kwin-screencast-` stream name) and the portal path assumptions are KWin-shaped. Mutter names its streams differently and exposes some first-class remote-desktop APIs; a GNOME port would generalize the predicate and prefer the native APIs where they exist. The cross-desktop options are in [screencast-detection-coverage.md](screencast-detection-coverage.md). Trigger: a GNOME user.

## Configuration & UX

- **Tray quick-toggle + richer indicator.** The tray indicator is read-only today. A quick manual arm/disarm toggle (routing through the same `remote-session start|stop` verb) and a per-source breakdown in the indicator are natural additions once there is demand.
- **GUI editing of the portal allow-list.** The attributed-exe allow-list (`krdpserver`, `rustdesk`, `sunshine`) is built in. Exposing it as an editable profile field would let users attribute other portal-using remote apps without a code change. Trigger: a portal-based remote app outside the list that a user wants attributed rather than falling back to the unattributed screencast signal.
- **Switch to disable the Sunshine poll.** The port is configurable (`--sunshine-port`), but there's no way to turn the `/serverinfo` poll off outright (a `StaticProbe` set to `Absent` already models it internally). Trigger: a user who wants the poll gone, e.g. Sunshine present but managed purely through its hooks.

## Testing & internals

- **`on_local_return` arm-debounce.** The return watch wakes on the first seat resume after a disconnect, which is seat activity, not proven human presence — a teardown input blip from the remote stack could in principle wake an empty room (the wake is idempotent, so the cost is one stray `screen_on`). A short debounce — ignore the resume for ~1–2 s after arming, since a real return lands seconds later while a teardown blip lands immediately — is the mitigation. Deferred: the first field QA (2026-07-18, Sunshine) saw none, a single return-wake ~1.7 s after the disconnect with no spurious fire. Trigger: a spurious post-disconnect wake observed in the field.
- **PipeWire fake for integration tests.** The PipeWire detector's glue is the one CI-untestable strip (the pure node predicate is unit-tested; the ThreadLoop/Registry wiring is verified live). A fake PipeWire graph seam would close the integration gap. Trigger: a regression in the glue that the pure test could not catch.
- **Greeter-session remote interplay.** Greeter events fire before any user session, so the armed set is empty and the gate is naturally inert at the login screen. If a remote session is ever established at the greeter (a remote login manager), the interaction with `greeter/idle` would need its own thought. No current use case.
