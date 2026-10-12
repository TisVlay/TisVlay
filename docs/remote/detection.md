# Remote Session Detection

How tisvlay learns that a remote desktop session (Sunshine/Moonlight, RustDesk, krdp/RDP, and anything hooked in by hand) is driving this machine, so it can suppress the TV-wake actions that injected input and un-DPMSed outputs would otherwise trigger for an empty room. The stock story: when a remote session arms a TV the panel blanks (`on_remote_connect` floors to `screen_off`), and when the session disarms the panel stays dark (`on_remote_disconnect` also floors to `screen_off`) — the room may still be empty, and a returning person wakes it through `on_local_return` (the first local input after the disconnect), with the now-disarmed unlock/lock-wake/idle-resume paths covering the cases where the desktop had locked or idled (see [Return after disconnect](#return-after-disconnect)). Detection, not policy, is the opt-in: nothing arms until a detector or hook fires, so the feature is inert on a stock desktop. While a TV is armed, arming is otherwise suppress-by-default: wake events are quiet out of the box, with off-going events (blanks and power-offs, including sleep/shutdown/reboot) exempt at the floor so a blank or an off-going power-down still lands. The per-event toggle expresses exceptions in either direction.

There is no first-class "a remote session is active" API on the Linux desktop, so detection is side-effect observation plus application hooks. The session service runs a set of independent detectors; each reports a tagged source into an aggregator, which crosses the live sources with each TV's resolved user profile to compute the per-TV `armed` set. The fact and its arming policy follow the project's fact/policy split: detection publishes raw truth, profiles own all behavior. See [architecture.md](../architecture.md) for the world/reactor model and the key space.

## Detector matrix

Each detector reports a source tagged by kind. Attributed kinds always arm; the unattributed screencast signal arms only when the resolved user profile opts in.

| Detector | Source kind | Attribution | Arms | Covers |
|----------|-------------|-------------|------|--------|
| Manual / hook verb | `manual` | the `--source` label | unconditionally | any scripted escape hatch, incl. the optional Sunshine `global_prep_cmd` hook |
| Sunshine level poll | `sunshine` | `serverinfo` | unconditionally | Sunshine's NvFBC/KMS path (zero-config, no hook needed) |
| Portal monitor | `portal` | caller exe (allow-list) | unconditionally, on `CreateSession` | krdp (both modes), RustDesk, allow-listed portal capture |
| PipeWire screencast | `screencast` | none | only with "Screen capture arms" on | any KWin-serviced capture (krdp, RustDesk, OBS, browser tab-share) |
| logind | `logind` | graphical session `Remote=yes` | unconditionally | xrdp-class RDP stacks that mint a real login session |

The manual verb (`tisvlay remote-session start|stop [--source NAME]`, or the SDK's `SessionClient::remote_session_set`) is the hook of last resort and the documented integration point for apps that expose no observable signal.

The PipeWire watcher matches KWin's capture node (`media.class = "Stream/Output/Video"`, `node.name = "kwin_wayland"`). It carries zero application attribution — it only knows a capture is running, not who started it — so it arms only when the profile's "Screen capture arms" switch is on. It does not see Sunshine's NvFBC/KMS capture path on Nvidia, which bypasses PipeWire entirely.

The watcher's client, `pipewire-native`, implements the protocol in Rust but still loads PipeWire's C support plugin (`support/libspa-support.so`), and it fixes two directories when it's built: the SPA plugin dir (from `pkg-config --variable plugindir libspa-0.2`, else `/usr/lib64/spa-0.2`) and the dir it reads `client.conf` from (`PIPEWIRE_CONFIG_DATA_DIR`, else `/usr/share/pipewire`). A build that doesn't match its host can't start the watcher: the Nix build takes pipewire as a build input and sets `PIPEWIRE_CONFIG_DATA_DIR` to its store path, and at startup the session checks that the support plugin exists among the plugin dirs (`SPA_PLUGIN_DIR` when set, else its own build's pkg-config answer and the FHS layouts) and leaves the watcher off with one warning when it doesn't, since the client would otherwise panic.

The portal monitor is a receive-only second connection to the session bus (`BecomeMonitor`) narrowly scoped to the portal object namespace. It parses ScreenCast/RemoteDesktop `CreateSession` calls (the earliest observable portal edge, before `Start` and before any injected input) and resolves the caller's unique bus name to a PID and then to the executable basename. Only an allow-listed exe (`krdpserver`, `rustdesk`, `sunshine`) reports an attributed `portal` source; a portal user outside the list falls back to the unattributed screencast signal. Because the monitor connection is receive-only, the sender lookup runs on the main session-bus connection.

The logind detector watches the system bus for own-uid sessions that are both `Remote=yes` and graphical (`Type=x11` or `Type=wayland`). Plain SSH sessions also mint `Remote=yes` logind sessions but are `Type=tty`, so they are excluded — the graphical type is what distinguishes a remote *desktop* from a remote shell, and only a desktop drives the outputs this arms against. Only xrdp-class stacks mint a qualifying session; krdp, RustDesk, and Sunshine are logind-invisible, so this detector is a supplement for RDP-family remoting, not a primary signal for the three main targets.

## Sunshine integration

Sunshine publishes no D-Bus interface and no push/event API, and its default NvFBC capture on Nvidia is invisible to both the portal monitor and the PipeWire watcher. Its one machine-readable state signal is the unauthenticated `/serverinfo` endpoint on loopback, which reports `SUNSHINE_SERVER_BUSY` while a stream is active and `SUNSHINE_SERVER_FREE` while idle. The session service runs a continuous poll against it, so Sunshine detection works **zero-config** — no hook required.

The poll gates on process presence (a cheap procfs scan, free when Sunshine is absent) and, while a `sunshine` process is running, issues a `GET http://127.0.0.1:<port>/serverinfo` on a short cadence (~2 s; a coarser ~10 s while no process is present), where `<port>` is Sunshine's base port: 47989 by default, set with `tisvlay-session --sunshine-port` (the NixOS module's `services.tisvlay.sunshinePort`) when Sunshine runs elsewhere, e.g. moved aside so Moonshine keeps the GameStream defaults. The poll and the eager pull below share one injected probe, which tests replace with a scripted one. It reports a `sunshine` source keyed by `serverinfo` that it fully owns: `BUSY` arms it, `FREE` or a vanished process disarms it, and a reachable-but-unclassifiable or transiently-unreachable endpoint keeps the last level so a blip does not flap the source. Reporting only on a level change keeps needless traffic off the aggregator. Because it re-derives the level every tick, a session-service restart mid-stream re-arms within one interval — no separate startup step. It needs Sunshine's HTTP port reachable on loopback; a firewall or a non-default port defeats it.

Because the disarm rides that ~2 s cadence, a user who returns within the window after a stream ends could hit a suppressed wake for up to one tick. The gate and the detector share the session process, so before dispatching a session-side event whose action a live Sunshine source is currently suppressing, the session pulls `/serverinfo` once on a tight budget and decides on fresh state — a stream that just ended un-gates the action immediately rather than waiting for the next poll. The trigger is policy-driven, not a fixed event list: for the stock floor it is exactly the wake events (unlock, lock-wake, idle-resume), but any event a profile gates is served the same. The world-side DPMS-on wake is cross-process and stays covered by the poll cadence; an eager refresh there is a filed follow-up.

The `global_prep_cmd` hook is now **optional**. Its sole remaining value is ordering: it arms the instant the app launches, ahead of the poll's interval latency and ahead of any injected input. Add one only if that sub-interval arm latency matters:

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

The CLI verb is fast and fire-and-forget by design, so a slow or stalled hook cannot delay Sunshine's stream launch. The hook fires at application launch, ahead of the stream and any injected input. It arms a **separate** `manual/sunshine` source, distinct from the poll's `sunshine/serverinfo` key, so the two coexist without racing: `/serverinfo` reads briefly `FREE` during the hook's `do` -> app-launch window, and a shared key would let the poll's disarm clear the hook's just-armed source. The aggregate is armed while either source is; both disarm on stream end.

Sunshine spawns hook commands outside the user's shell environment, so unpackaged setups (a dev checkout, a manually installed CLI) must use an absolute path to the `tisvlay` binary and pass the socket flags explicitly; edits to `global_prep_cmd` take effect on the next Sunshine restart. The hook brackets the app's lifetime, not the client's: a Moonlight reconnect to a still-running app re-runs nothing (the source is already armed), and the source disarms when the app quits, not when a client merely disconnects.

One residual remains when the hook is configured: a full Sunshine *crash* strands the hook's `manual/sunshine` source until the next session restart — the poll cannot clear the hook's key without racing the hook, so it only ever clears its own `sunshine/serverinfo` key. It is narrow and self-clears on restart. Without a hook there is nothing to strand: the poll owns both arm and disarm.

## Return after disconnect

The disconnect blank assumes the room is empty, but the person may simply have walked over to the physical machine. If they had locked or let the desktop idle, the normal unlock/lock-wake/idle-resume path wakes the TV on return. If they did neither — unlocked and not yet idle — no such edge exists (the local output never DPMS-off'd either, so the world-side DPMS-on path has nothing to react to), and the panel would stay stranded dark. `on_local_return` closes that window: the session arms a return watch on the disconnect and wakes the TV (floor: `screen_on`, restoring input) on the first genuine local input afterward. Detection reuses the always-on 200 ms lock-wake idle probe — once the injected input stops the seat idles within ~200 ms, and the user's first real input resumes it. The watch clears on the next session event (the wake itself, a lock, an idle timeout, or a new remote connect), so it fires once and the normal paths own everything after. A profile can set `on_local_return` to `none` to opt out.

Two limitations follow from the signal. A user inputting *continuously* across the disconnect (no ~200 ms gap for the seat to idle) is not seen until they next pause and move; that miss window is as wide as the disconnect-detection latency (up to the Sunshine poll interval). And the probe watches the seat, not the human, so a residual input event from the remote stack's teardown could wake an empty room — the wake is idempotent, and a short arm-debounce is the mitigation if field use shows it.

## False positives and filtering

InputCapture portal sessions never arm. Software-KVM tools such as lan-mouse hold a standing InputCapture session while the user is physically present; InputCapture shares the portal object path and the `CreateSession` member with the capture interfaces, so it is excluded by interface unconditionally rather than by heuristic.

OBS recording and browser tab-share produce a KWin capture node but no attribution, so they arm only when "Screen capture arms" is on. With that switch off (the default) they are ignored; with it on, any screencast arms, which is the intended trade-off for users who want capture itself to suppress TV wake.

## Bootstrap and restart

The fact is hydratable, so a service restart mid-session re-arms without waiting for the next detector edge.

- The PipeWire watcher enumerates the graph at startup and rediscovers a live capture node.
- The portal monitor tree-walks `/org/freedesktop/portal/desktop/session/*`; the session-object path segment encodes the owner's bus name, which resolves to an exe. Session objects are interface-blind (an InputCapture session looks identical to a capture session), so a bootstrapped allow-listed owner is admitted only provisionally, and enters the live set only while a screencast source corroborates a real capture. A `CreateSession` later seen directly on the wire supersedes the guess and arms unconditionally.
- The logind detector lists current sessions and reports any own-uid graphical `Remote=yes` session.
- A session-service restart therefore re-arms via graph and session enumeration; a daemon restart re-arms via the mirror re-put (the session re-publishes the fact when the daemon's liveliness token reappears, since the system-socket mirror is a plain put over a connect-only peer). The reverse presence signal is a `session_alive` liveliness token the session declares on that same link: the daemon clears the armed set the instant the token deletes — a session crash (`kill -9`) as well as a clean exit — so a departed session never leaves a TV latched armed. The daemon also holds its own boot Wake-on-LAN dispatch until that armed state is known (a mirror sample seen, or no session token past a short settle, or a fallback deadline), so a boot while a session is remoted does not wake a TV before the mirror arrives.

Sunshine's NvFBC path is the exception: with no portal session and no PipeWire node to rediscover, the hook alone cannot re-arm it (it fires once, at launch). The continuous `/serverinfo` poll covers it instead — because it re-derives the level every tick, a session-service restart mid-stream re-arms the `sunshine/serverinfo` source within one interval, with no separate startup step (see [Sunshine integration](#sunshine-integration)).

## Degradation

Each detector is independently supervised. When its bus or graph is absent — no session bus, no system bus, no PipeWire, `BecomeMonitor` denied — the detector warns once and idles with a supervised retry loop, so a missing subsystem degrades that one signal quietly rather than failing the session service. `BecomeMonitor` rides the session bus and is owner-allowed under both dbus-daemon and dbus-broker, so no packaged bus policy is required.

With no session service at all, the armed set is empty and nothing is gated — a headless TV with no user session never arms, which is intended.

## Observability

The fact lives on the user socket at `tisvlay/<site>/user/remote/session` (watch it by pointing `watch` at the user socket: `tisvlay --socket <user-sock> watch`) and is mirrored to the system socket at `tisvlay/<site>/remote/session`, visible in a plain `tisvlay watch`. Both carry the live `sources` (each tagged by kind, with a `detail` naming the specific origin) and the computed `armed` TV list. `tisvlay remote-session status` prints the current fact.

The per-TV armed state also rides each TV's meta projection, so it surfaces in TV state and drives the tray's read-only "Remote session active" indicator (shown while any TV is armed). Behavior policy lives in the profile: whether an unattributed screen capture arms is the "Screen capture arms" switch (the GUI's REMOTE SESSION card, and `remote_screencast_arms` in the profile TOML), and whether an individual event fires while armed is the per-event `while_remote` gate — a network toggle on each action row in the GUI's FIELDS card, or `while_remote = true` on the event's action table. The gate is suppress-by-default: a bare action (or any action omitting the field) is quiet while the TV is armed, so an unconfigured profile keeps the panel dark for the empty room; `while_remote = true` opts an event back into firing. The floor splits by event direction: wake events are suppressed while armed (so a wake for an empty room is quiet — startup, unlock, DPMS-on, and the resume mirrors), while blanks and power-offs still fire (a blank is always safe to let through, and the machine going away should still turn the TV off — the DPMS-off/lock/idle-timeout blanks alongside sleep, shutdown, and reboot). The gate is pure suppression: it only ever stops an action that would already fire, never causes one an unarmed TV would not take.
