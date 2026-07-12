# Remote Session Detection

How tisvlay learns that a remote desktop session (Sunshine/Moonlight, RustDesk, krdp/RDP, and anything hooked in by hand) is driving this machine, so profiles can suppress the TV-wake actions that injected input and un-DPMSed outputs would otherwise trigger for an empty room.

There is no first-class "a remote session is active" API on the Linux desktop, so detection is side-effect observation plus application hooks. The session service runs a set of independent detectors; each reports a tagged source into an aggregator, which crosses the live sources with each TV's resolved user profile to compute the per-TV `armed` set. The fact and its arming policy follow the project's fact/policy split: detection publishes raw truth, profiles own all behavior. See [architecture.md](../architecture.md) for the world/reactor model and the key space.

## Detector matrix

Each detector reports a source tagged by kind. Attributed kinds always arm; the unattributed screencast signal arms only when the resolved user profile opts in.

| Detector | Source kind | Attribution | Arms | Covers |
|----------|-------------|-------------|------|--------|
| Manual / hook verb | `manual` | the `--source` label | unconditionally | Sunshine `global_prep_cmd`, any scripted escape hatch |
| Portal monitor | `portal` | caller exe (allow-list) | unconditionally, on `CreateSession` | krdp (both modes), RustDesk, allow-listed portal capture |
| PipeWire screencast | `screencast` | none | only with "Screen capture arms" on | any KWin-serviced capture (krdp, RustDesk, OBS, browser tab-share) |
| logind | `logind` | session `Remote=yes` | unconditionally | xrdp-class RDP stacks that mint a real login session |

The manual verb (`tisvlay remote-session start|stop [--source NAME]`, or the SDK's `SessionClient::remote_session_set`) is the hook of last resort and the documented integration point for apps that expose no observable signal.

The PipeWire watcher matches KWin's capture node (`media.class = "Stream/Output/Video"`, `node.name = "kwin_wayland"`). It carries zero application attribution — it only knows a capture is running, not who started it — so it arms only when the profile's "Screen capture arms" switch is on. It does not see Sunshine's NvFBC/KMS capture path on Nvidia, which bypasses PipeWire entirely.

The portal monitor is a receive-only second connection to the session bus (`BecomeMonitor`) narrowly scoped to the portal object namespace. It parses ScreenCast/RemoteDesktop `CreateSession` calls (the earliest observable portal edge, before `Start` and before any injected input) and resolves the caller's unique bus name to a PID and then to the executable basename. Only an allow-listed exe (`krdpserver`, `rustdesk`, `sunshine`) reports an attributed `portal` source; a portal user outside the list falls back to the unattributed screencast signal. Because the monitor connection is receive-only, the sender lookup runs on the main session-bus connection.

The logind detector watches the system bus for own-uid sessions with `Remote=yes`. Only xrdp-class stacks mint such a session; krdp, RustDesk, and Sunshine are logind-invisible, so this detector is a supplement for RDP-family remoting, not a primary signal for the three main targets.

## Sunshine integration

Sunshine publishes no D-Bus interface and no push/event API, and its default NvFBC capture on Nvidia is invisible to both the portal monitor and the PipeWire watcher. The reliable, event-driven signal is its command hooks. Add a `global_prep_cmd` that arms on setup (`do`) and disarms on teardown (`undo`):

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

The CLI verb is fast and fire-and-forget by design, so a slow or stalled hook cannot delay Sunshine's stream launch. The hook fires at application launch, ahead of the stream and any injected input, which is the ordering the gate needs.

Sunshine spawns hook commands outside the user's shell environment, so unpackaged setups (a dev checkout, a manually installed CLI) must use an absolute path to the `tisvlay` binary and pass the socket flags explicitly; edits to `global_prep_cmd` take effect on the next Sunshine restart. The hook brackets the app's lifetime, not the client's: a Moonlight reconnect to a still-running app re-runs nothing (the source is already armed), and the source disarms when the app quits, not when a client merely disconnects.

A localhost `/serverinfo` poller (the unauthenticated `SUNSHINE_SERVER_BUSY`/`_FREE` endpoint) is a candidate zero-config fallback that would also re-arm a mid-session restart of the NvFBC path — the hook alone cannot, because there is no portal session or PipeWire node to rediscover. Whether it ships, and at what poll interval, is a pending decision gated on bench measurement of its latency and wakeup cost versus the hook.

## False positives and filtering

InputCapture portal sessions never arm. Software-KVM tools such as lan-mouse hold a standing InputCapture session while the user is physically present; InputCapture shares the portal object path and the `CreateSession` member with the capture interfaces, so it is excluded by interface unconditionally rather than by heuristic.

OBS recording and browser tab-share produce a KWin capture node but no attribution, so they arm only when "Screen capture arms" is on. With that switch off (the default) they are ignored; with it on, any screencast arms, which is the intended trade-off for users who want capture itself to suppress TV wake.

## Bootstrap and restart

The fact is hydratable, so a service restart mid-session re-arms without waiting for the next detector edge.

- The PipeWire watcher enumerates the graph at startup and rediscovers a live capture node.
- The portal monitor tree-walks `/org/freedesktop/portal/desktop/session/*`; the session-object path segment encodes the owner's bus name, which resolves to an exe. Session objects are interface-blind (an InputCapture session looks identical to a capture session), so a bootstrapped allow-listed owner is admitted only provisionally, and enters the live set only while a screencast source corroborates a real capture. A `CreateSession` later seen directly on the wire supersedes the guess and arms unconditionally.
- The logind detector lists current sessions and reports any own-uid `Remote=yes` session.
- A session-service restart therefore re-arms via graph and session enumeration; a daemon restart re-arms via the mirror re-put (the session re-publishes the fact when the daemon's liveliness token reappears, since the system-socket mirror is a plain put over a connect-only peer).

Sunshine's NvFBC path is the exception: with no portal session and no PipeWire node to rediscover, only the poller could re-arm it after a restart — the hook fires once, at launch.

## Degradation

Each detector is independently supervised. When its bus or graph is absent — no session bus, no system bus, no PipeWire, `BecomeMonitor` denied — the detector warns once and idles with a supervised retry loop, so a missing subsystem degrades that one signal quietly rather than failing the session service. `BecomeMonitor` rides the session bus and is owner-allowed under both dbus-daemon and dbus-broker, so no packaged bus policy is required.

With no session service at all, the armed set is empty and nothing is gated — a headless TV with no user session never arms, which is intended.

## Observability

The fact lives on the user socket at `tisvlay/<site>/user/remote/session` (watch it with `tisvlay watch --user-socket <path>`) and is mirrored to the system socket at `tisvlay/<site>/remote/session`, visible in a plain `tisvlay watch`. Both carry the live `sources` (each tagged by kind, with a `detail` naming the specific origin) and the computed `armed` TV list. `tisvlay remote-session status` prints the current fact.

The per-TV armed state also rides each TV's meta projection, so it surfaces in TV state and drives the tray's read-only "Remote session active" indicator (shown while any TV is armed). Behavior policy lives in the profile: whether an unattributed screen capture arms is the "Screen capture arms" switch (the GUI's REMOTE SESSION card, and `remote_screencast_arms` in the profile TOML), and whether an individual event fires while armed is the per-event `while_remote` gate — a network toggle on each action row in the GUI's FIELDS card, or `while_remote = false` on the event's action table. The gate is pure suppression: it only ever stops an action that would already fire, never causes one an unarmed TV would not take. It is honored on every event; the recommendation is to leave sleep and shutdown ungated so an off-going power-down still lands during a remote session.
