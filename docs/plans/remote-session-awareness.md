# Remote session awareness

## Context

When the user remotes into this machine (Sunshine/Moonlight game streaming, RustDesk, krdp/RDP — and hooks for anything else), the remote session unlocks the desktop and injected virtual input resets KWin idle and un-DPMSes the outputs. To tisvlay those look exactly like a person sitting down: unlock, idle-resume, and DPMS-on actions fire and the physical TV powers on for an empty room. The feature: tisvlay learns "a remote session is active" as a per-user **fact**, and **profiles** decide behavior — configurable actions on remote connect/disconnect, plus per-event overrides while remote is active. Follows the project's fact/policy split: detection publishes raw truth, profiles own all behavior.

Research fan-out (2026-07-11, six agents: Sunshine/RustDesk/krdp source, LGTVCompanion prior art, live platform introspection, tisvlay integration map) established there is **no first-class "remote session active" API on the Linux desktop** — detection is side-effect observation plus app hooks:

- **PipeWire graph**: any KWin-serviced capture = node `media.class="Stream/Output/Video"`, `node.name="kwin_wayland"`, state `RUNNING`. Covers krdp (both modes) and RustDesk; **not** Sunshine's NvFBC/KMS default on Nvidia; carries **zero app attribution**.
- **D-Bus portal monitor**: portal `Request.Response`/`Session.Closed` are unicast (xdp ≥ 1.18.1), but a `BecomeMonitor` receive-only connection sees ScreenCast/RemoteDesktop `Start`/`Closed` — live-proven on this box — and the sender resolves to PID → exe for attribution.
- **Sunshine**: portal-invisible by default on Nvidia (uinput input, NvFBC capture, no inhibitors, no D-Bus). Reliable signals: its `prep-cmd`/`global_prep_cmd` hooks (→ our CLI verb) and the unauthenticated `GET http://127.0.0.1:47989/serverinfo` `SUNSHINE_SERVER_BUSY|_FREE` endpoint.
- **logind**: only xrdp-class stacks mint `Remote=yes` sessions; the three target apps are logind-invisible.
- **Live false-positive specimen**: lan-mouse holds a standing portal **InputCapture** session on this box while the user is physically present — InputCapture-only sessions must never count.
- **Prior art** (LGTVCompanion, Windows — the only project with this feature): OR-aggregate of per-source detectors, edge-triggered with asymmetric debounce, gates display/idle events but never sleep/shutdown, manual CLI escape hatch, restores HDMI input on disconnect. No Linux prior art exists.

## Decisions (user-confirmed 2026-07-11)

1. **Scope: system + session.** The session service detects and publishes the fact; a mirror on the system socket lets the world gate `dispatch_system_events` (DPMS-on is the main TV-wake path). Sleep/shutdown/startup paths are **never** gated (LGTVCompanion lesson).
2. **Fact model A: one fact, source-kind tags.** The fact publishes all live sources tagged by kind (`manual` / `sunshine` / `logind` / `portal:<exe>` / `screencast`), no baked-in aggregate. Attributed kinds always arm; one profile switch (`remote_capture_arms`, default off) decides whether the unattributed screencast signal also arms. Alternatives considered and dropped: attributed-only (no fallback when attribution misses, no capture-suppression option) and two separate facts (double schema/wire/GUI surface, precedence rule, no motivating use case for distinct capture actions — model A's tagged payload migrates forward without a wire break if one appears).
3. **Policy shape: while-remote override block** — per-event override fields remapping event→action while armed (unset = inherit the normal action, `none` = suppress) + `on_remote_connect`/`on_remote_disconnect` as first-class events. Arming policy lives in **user** profiles only; system profiles carry only their override actions.
4. **v1 detectors (all session-side)**: manual/hook RPC verb, PipeWire screencast watcher, D-Bus portal monitor, Sunshine `/serverinfo` poller, logind Remote watcher.
5. **PipeWire crate: `pipewire-native` git-pinned** to a current upstream rev (crates.io 0.1.4 aborts the process at an FFI-callback boundary on protocol decode errors vs PipeWire ≥ 1.4; the fix is unreleased and upstream tags rarely). One documented `allow-git` entry in `packaging/licensing/deny.toml`, revert-to-crates.io when 0.1.5+ ships. Verify the pinned rev against PipeWire 1.6 during implementation.
6. **Debounce**: arming is immediate-on / delayed-off; the connect/disconnect **actions** debounce (defaults 10 s / 1 s; profile knobs). One shared delayed-off knob covers both disarm and the disconnect action.

## Architecture

```
detectors (session service, independent tasks)          policy (profiles)
  verb rpc ─┐                                             user: arming + events + overrides
  pipewire ─┤   aggregator (remote.rs)                    system: overrides only
  monitor ──┼─► sources: Vec<RemoteSource>  ──resolve──►  armed(tv) per user profile
  sunshine ─┤        │                                         │
  logind ───┘        │ cached pub                              │
                     ▼                                         ▼
   user socket   tisvlay/<site>/user/remote_session   Core::dispatch gate (session actions)
                     │ mirror (client link, like event/session/* taps)
                     ▼
   system socket  tisvlay/<site>/remote_session ──► daemon subscriber ──► InEvent::RemoteSession
                                                          │
                                                          ▼
                                    world RemoteArmed marker/resource ──► dispatch_system_events gate
```

- **Arming is computed session-side** from sources × the per-TV resolved user profile (`remote_capture_arms`), because arming is a user-session policy question. The mirror payload carries both raw `sources` and the computed `armed: Vec<Uuid>` so the world consumes a boolean per TV and stays arming-policy-free.
- **Gates are additive to `automated`/`automate_when`** — orthogonal checks, never a replacement (avoid double-suppression interplay).
- The fact is **hydratable**: the PipeWire watcher enumerates the graph at startup, the monitor bootstraps via a portal session-object tree-walk (owner exe + video-node presence heuristic join), the Sunshine poller reads current state — a session-service restart mid-remote-session re-arms.

## Implementation steps

### 1. Wire vocabulary (`crates/tisvlay-wire/src/lib.rs`)

- `keys`: `user_remote_session(site)` → `tisvlay/<site>/user/remote_session` (cached pub, user socket); `remote_session(site)` → `tisvlay/<site>/remote_session` (mirror, system socket); `user_remote_rpc(site, verb)` + `_wild` + `parse_` (RPC family on the user socket).
- `payload`: `RemoteSessionState { sources: Vec<RemoteSource>, armed: Vec<Uuid> }`, `RemoteSource { kind: RemoteSourceKind, detail: String }`, `RemoteSourceKind { Manual, Sunshine, Logind, Portal, Screencast }`. New cached pubs follow the existing hygiene: `sample_miss_detection` + `publisher_detection`, liveliness declared after serving.

### 2. Profile schema (`crates/tisvlay-core/src/config.rs`, 3-edit macro pattern per field)

User profile (`UserProfileConfig` :431, macro `fields:[]` :472, `ResolvedUserProfile` + `Default` :497/:514):
`on_remote_connect`, `on_remote_disconnect` (`Option<EventAction>`, default None), `remote_connect_delay_seconds` (default 10), `remote_disconnect_delay_seconds` (default 1), `remote_capture_arms` (`Option<bool>`, default false), `while_remote_on_session_unlock`, `while_remote_on_lock_wake`, `while_remote_on_idle_resume` (`Option<EventAction>`, default None — flat fields, macro-native merge, and the GUI field-table renders them directly).

System profile (`SystemProfileConfig` :288, macro :341, `ResolvedSystemProfile` :370/:395):
`while_remote_on_dpms_on`, `while_remote_on_wake`, `while_remote_on_dpms_off` (`Option<EventAction>`, default None). Stock defaults keep every new field neutral — the feature is opt-in via profile editing (PR #34 precedent: novel behavior ships opt-in). No versioning/migration (absence defines v1).

### 3. Session service (`crates/tisvlay-session`)

- New `remote.rs` module: aggregator task owning source state; detectors report over an `mpsc`; recomputes `armed` on source or profile changes (profile watch like `idle_timeouts`); publishes the user-socket fact + system-socket mirror; drives debounced `SessionEvent::RemoteConnect`/`RemoteDisconnect` into `Core` (extend the enum at `lib.rs:48` + `tap_name()`).
- Detector tasks (each copies the `watch_screensaver` shape — spawn in `main.rs`, report into the aggregator):
  - **verb**: extend `Core::route` (`lib.rs:577`) with the `user_remote_rpc` family (`set {source, active}` / `status`); inline replies like the profile verbs.
  - **pipewire**: dedicated OS thread (pipewire-native `ThreadLoop` + `Core` + `Registry`; Context held alongside Core — PR #34 `volume_sync.rs` pattern), registry `global`/`global_remove` + node-info listeners, predicate `media.class == "Stream/Output/Video" && node.name == "kwin_wayland" && state == RUNNING` factored as a pure fn; reports the `Screencast` source; enumerates at startup (hydration).
  - **monitor**: second zbus connection → `org.freedesktop.DBus.Monitoring.BecomeMonitor` with match rules on the portal path namespace; parses ScreenCast/RemoteDesktop `CreateSession`/`Start`/`Closed` (pure parser fn over message fixtures); sender → `GetConnectionUnixProcessID` on the **main** connection (monitor connections are receive-only) → `/proc/<pid>/exe` → built-in allow-list (`krdpserver`, `rustdesk`, `sunshine`) → `Portal` source; **InputCapture interface ignored unconditionally**; bootstrap tree-walk of `/org/freedesktop/portal/desktop/session/*` joins owner exe × video-node presence.
  - **sunshine**: 3 s localhost poll of `:47989/serverinfo` gated on a cheap procfs presence check for the sunshine process; `SUNSHINE_SERVER_BUSY` → `Sunshine` source.
  - **logind**: system-bus zbus watch of `SessionNew`/`SessionRemoved` + `Remote` property for own-uid sessions → `Logind` source.
- Gating: in `Core::dispatch`/`action_for` (`lib.rs:426+`), when `armed(tv)`, resolve the while-remote override for Unlock/LockWake/IdleResume before `call_tv` — beside the existing checks, additive to `automated`.

### 4. Daemon + world

- Daemon: subscribe to the mirror key (greeter pattern, `crates/tisvlay-daemon/src/lib.rs:274`) → forward a new `InEvent::RemoteSession { state }` to the world.
- World (`crates/tisvlay-world`): `RemoteArmed` resource/marker updated from the event; gate in `dispatch_system_events` (`systems.rs:1959`, beside `is_automated`): for armed TVs, resolve the while-remote system-profile override for `DpmsOn`/`Resume`/`DpmsOff` (unset → normal, `none` → skip). `Sleep`/`Shutdown`/`Startup` arms untouched. Publish the armed projection for clients (`publish_changed` chain, `systems.rs:58`) so the tray/GUI can indicate it.

### 5. SDK, CLI, tray

- SDK (`crates/tisvlay/src/lib.rs` + `mirror.rs`): fact in `UserMirror` + `StateWatcher` surface; `SessionClient::remote_session_set/status`.
- CLI (`crates/tisvlay-cli/src/main.rs`): `tisvlay remote-session start|stop|status [--source NAME]` — the documented Sunshine `global_prep_cmd` integration (`do` = start, `undo` = stop; fast, fire-and-forget so a slow hook can't stall Sunshine's launch).
- Tray (`crates/tisvlay-tray`): read-only "Remote session active" indicator item following the `managed_toggle` shape; quick-toggle deferred.

### 6. GUI (`crates/tisvlay-gui`)

- Extend `USER_FIELDS`/`SYSTEM_FIELDS` + `field_label` + read/write match arms (`src/profiles.rs`); new `WHILE_REMOTE_FIELDS` table rendering as a second card below FIELDS in `ui/components/profiles_pane.slint` (uppercase mini-title convention; Switch row for `remote_capture_arms`; same row anatomy — gutter accent, reset glyph, restore-input on wake actions). Field-table tests validate against `*_PROFILE_FIELDS`.
- Chosen pane (user profile; system profiles get the parallel override card for DPMS/wake rows):

```
USER PROFILE                                    [⧉] [↻]
┌ FIELDS ────────────────────────────────────────────┐
│ On session lock         [ screen_off       ▾ ]     │
│ …existing rows…                                    │
│ On idle resume          [ screen_on        ▾ ] ⟳   │
│ On remote connect       [ none             ▾ ]  NEW│
│ On remote disconnect    [ none             ▾ ]  NEW│
│ Remote connect delay    [   10 ▴▾ ] seconds     NEW│
│ Remote disconnect delay [    1 ▴▾ ] seconds     NEW│
└────────────────────────────────────────────────────┘
WHILE REMOTE ACTIVE  ⓘ                            NEW
┌────────────────────────────────────────────────────┐
│ Screen capture arms       ⭘ off   (Switch)         │
│   · remote apps / hooks / logind always arm        │
│   · on: any screencast (OBS, tab-share) also arms  │
│ On session unlock       [ none             ▾ ]     │
│ On lock wake            [ none             ▾ ]     │
│ On idle resume          [ none             ▾ ]     │
│   unset override row = behave as normal            │
└────────────────────────────────────────────────────┘
```

- New strings via `@tr()` / `pgettext("profile field", …)`; run `just i18n-extract` and commit the `.pot` diff.

### 7. Packaging

- `packaging/licensing/deny.toml`: `allow-git = ["https://gitlab.freedesktop.org/pipewire/pipewire-native-rs.git"]` with the rationale + revert condition commented at the entry (single canonical comment; the Cargo.toml pin references nothing else).
- cargo-about/NOTICE pass for the new dependency tree (`just licenses`).

## Testing

- **Pure fns** (unit): arming computation (sources × capture-arms policy), debounce edges (immediate-on/delayed-off, action delays), while-remote override resolution, the PipeWire node predicate (struct fixtures), the portal-monitor message parser (captured-message fixtures) — same factoring discipline as `idle_dispatch`/`lock_wake_policy`.
- **Session integration** (`crates/tisvlay-session/tests/session.rs` `Rig`): inject detector reports through the aggregator channel (test seam) and `SessionEvent::RemoteConnect/Disconnect` via `client.send`; assert mock-TV actions, override suppression of idle-resume/unlock, fact + mirror key contents. Zenoh tests stay `#[tokio::test(flavor = "multi_thread")]`; waits use combined predicates in a single drain.
- **Daemon/world integration** (`crates/tisvlay-daemon/tests/`): mirror-key consumption → world gate honored for DpmsOn (greeter-test pattern); sleep path unaffected while armed.
- **Wire/SDK** (`crates/tisvlay/tests/client.rs`): fact hydration from cache, RPC verb round-trip via `Caller` (no-reply retry).
- **Fakes**: fake logind exists (`tisvlay-test`); a fake portal-monitor source is just the parser fixtures + channel injection; PipeWire is covered by the pure predicate + a manual live check (no fake graph in v1 — documented gap).

## Live verification (user-assisted, real hardware)

1. krdp session from another machine: fact arms (`portal:krdpserver` + `screencast`), unlock/idle-resume/DPMS-on suppressed per overrides, TV stays off; disconnect → disarm after delay, `on_remote_disconnect` action fires.
2. RustDesk: same expectations via portal attribution.
3. Sunshine + Moonlight (NvFBC path): arms via `/serverinfo` poller and/or the documented prep-cmd hook; confirm Moonlight input trips ext-idle-notify unsuppressed *before* the feature and suppressed after.
4. False-positive probes: OBS recording and a browser tab-share with `remote_capture_arms` off (no arming) and on (arms); lan-mouse present throughout (must never arm); Plasma thumbnails/Overview spot-check for transient `Stream/Output/Video` nodes.
5. `tisvlay remote-session start` / `stop` round-trip; `tisvlay watch` shows fact transitions; session-service restart mid-krdp-session re-arms (hydration).

## Deferred (file into docs/ideas/ on merge)

Tray quick-toggle + richer indicator; GUI editing of the portal allow-list; GNOME portability (mutter node names, first-class Mutter APIs); Steam Remote Play / Parsec detectors; configurable Sunshine endpoint; greeter-session remote interplay; PipeWire fake for integration tests; journald-tail detectors (rejected for brittleness — revisit only if a target app loses its signal).
