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

## Decisions (user-confirmed 2026-07-11; review pass applied)

1. **Scope: system + session.** The session service detects and publishes the fact; a mirror on the system socket lets the world gate `dispatch_system_events` (DPMS-on is the main TV-wake path). Sleep/shutdown/startup paths are **never** gated (LGTVCompanion lesson), and greeter events (`GreeterIdle`/`GreeterResume`) are exempt too — they fire at the login screen, before any user session or remote fact exists.
2. **Fact model A: one fact, source-kind tags.** The fact publishes all live sources tagged by kind (`manual` / `sunshine` / `logind` / `portal:<exe>` / `screencast`), no baked-in aggregate. Attributed kinds always arm; one profile switch (`remote_screencast_arms`, default false — GUI label "Screen capture arms") decides whether the unattributed screencast signal also arms. Alternatives considered and dropped: attributed-only (no fallback when attribution misses, no capture-suppression option) and two separate facts (double schema/wire/GUI surface, precedence rule, no motivating use case for distinct capture actions — model A's tagged payload migrates forward without a wire break if one appears).
3. **Policy shape: while-remote override block** — per-event override fields remapping event→action while armed (unset = inherit the normal action, `none` = suppress) + `on_remote_connect`/`on_remote_disconnect` as first-class events. **Overrides cover wake-ish events only** (suppressing an off-going blank during remote is never wanted) and they **only ever replace an action that would fire — they can never cause an action an unarmed TV wouldn't take** (otherwise remote-connect becomes a wake vector; "switch the TV to a desired state" rides on `on_remote_connect`, not on overrides). Arming policy lives in **user** profiles only; system profiles carry only their override actions — never `on_remote_*` events (the connect/disconnect events are session-detected and session-dispatched). **SUPERSEDED after the working prototype — see "Refactor: while_remote as an action subfield" below.**
4. **v1 detectors (all session-side)**: manual/hook RPC verb, PipeWire screencast watcher, D-Bus portal monitor, Sunshine `/serverinfo` poller, logind Remote watcher. Sunshine offers **no subscription surface** (verified in source: no D-Bus published, no event/push API — only hooks, the poll endpoint, and log lines), so the event-driven Sunshine path is the documented `prep-cmd` hook; the poller is the zero-config fallback *(Phase 0-conditional: keep / demote to opt-in / drop)*. Candidate event-driven alternative to evaluate on the bench: a udev monitor for Sunshine's per-client uinput devices (gamepad/touch/pen are created per session; keyboard/mouse are persistent, so coverage is partial).
5. **PipeWire crate: `pipewire-native` git-pinned** to a current upstream rev (crates.io 0.1.4 aborts the process at an FFI-callback boundary on protocol decode errors vs PipeWire ≥ 1.4; the fix is unreleased and upstream tags rarely). One documented `allow-git` entry in `packaging/licensing/deny.toml`, revert-to-crates.io when 0.1.5+ ships. Verify the pinned rev against PipeWire 1.6 during implementation. The pin drags three sibling crates from one URL (`pipewire-native`, `-spa`, `-macros`; all MIT — no license exceptions needed).
6. **Debounce**: arming is immediate-on / delayed-off; the connect/disconnect **actions** debounce. Whether action debounce is merited at all — and the defaults if so (LGTVCompanion ships fixed 10 s / 1 s on Windows; that's their event model, not ours) — is **pending Phase 0 bench measurement**. The delay fields (`remote_connect_delay_seconds` / `remote_disconnect_delay_seconds`) **do not enter the schema, macro tables, or GUI until Phase 0 shows real flapping** — removing them later would be a coordinated multi-site revert across two crates; adding them is a small follow-up commit.

## Architecture

```
detectors (session service, independent tasks)          policy (profiles)
  verb rpc ─┐                                             user: arming + events + overrides
  pipewire ─┤   aggregator (remote.rs)                    system: overrides only
  monitor ──┼─► sources: Vec<RemoteSource>  ──resolve──►  armed(tv) per user profile
  sunshine ─┤        │                                         │
  logind ───┘        │ cached pub (user socket = session-owned router)
                     ▼                                         ▼
   user socket   tisvlay/<site>/user/remote/session   Core::action_for gate (session actions)
                     │ plain put (the greeter-idle shape; NOT a cached pub —
                     │ the session's system link is a connect-only peer, and served
                     │ entities do not propagate over it; re-put on daemon liveliness)
                     ▼
   system socket  tisvlay/<site>/remote/session ──► daemon bare subscriber ──► InEvent::RemoteSession
                                                          │
                                                          ▼
                                    world RemoteArmed resource ──► dispatch_system_events gate
```

- **Arming is computed session-side** from sources × the per-TV resolved user profile (`remote_screencast_arms`), because arming is a user-session policy question. The mirror payload carries both raw `sources` and the computed `armed: Vec<Uuid>` so the world consumes booleans per TV and stays arming-policy-free. **Invariant: no session service → empty armed set → nothing gated** (a headless TV with no user session never arms; that's intended). Multiple concurrent user sessions per seat are out of scope (one session service per user socket). Mirror re-publishes are debounced against profile-watch storms.
- **Gates are additive to `automated`/`automate_when`** — orthogonal checks, never a replacement (avoid double-suppression interplay).
- The fact is **hydratable**: the PipeWire watcher enumerates the graph at startup, the monitor bootstraps via a portal session-object tree-walk (owner exe + video-node presence heuristic join), the Sunshine poller reads current state — a session-service restart mid-remote-session re-arms. **Hydration completeness for Sunshine's NvFBC path depends on the poller** (no portal session, no PipeWire node to rediscover); the hook alone cannot re-arm after a restart. Daemon-side: the mirror is a plain put, so the session re-puts current state when the daemon's liveliness token (re)appears — a daemon restart mid-session re-arms without waiting for the next detector edge.

### Event-ordering races (user-flagged; resolved by Phase 0)

The gate only helps if a remote source is seen **before** the events it must override: an RDP-injected unlock could reach `Core::dispatch` before the portal monitor reports the session, and KWin can flip DPMS-on the moment capture or injected input starts. Event stalling is explicitly out — we never delay event dispatch to wait for corroborating signals. Non-stalling mitigation candidates, preference order:

- **Arm on the earliest observable signal**: the monitor arms on `CreateSession` (precedes `Start` and any injected input); the PipeWire watcher treats node *appearance* (even pre-RUNNING) as arming-eligible; Sunshine's hook fires at app launch, before the stream starts.
- **Retroactive correction** (only if the bench shows real misses): if a wake-ish action fired and a remote source arms within a short window, apply the compensating while-remote action (e.g. screen back off). Bounded window, off by default.
- Expected orderings favor us for krdp/RustDesk — portal session setup precedes the human typing their password by seconds — but Phase 0 measures instead of assuming.

## Implementation steps

### 0. Bench measurements (before code freezes the knobs)

User-assisted instrumentation on the real system — timestamped traces (`busctl --user monitor` on the portal namespace, `pw-mon`, `journalctl -f`, `tisvlay watch`, dpms sysfs) across ≥3 connect/disconnect cycles each for krdp, RustDesk, and Sunshine/Moonlight:

- **Ordering**: remote signal (`CreateSession`/`Start`/node-add/hook/poll flip) vs ScreenSaver unlock vs dpms sysfs flip vs idle-resume — sizes the race windows and decides whether retroactive correction ships.
- **Debounce**: how connect/disconnect edges actually flap during stream negotiation and Moonlight reconnects — decides whether action debounce is merited; if no flapping, the delay fields never enter the schema.
- **Sunshine poller**: latency vs the prep-cmd hook, wakeup cost at 3–5 s intervals, and the udev per-client-device candidate — keep, demote to opt-in, or drop.

Both outcomes of each question are pre-approved above (Decisions 4 and 6): the freeze is a selection, not a re-draft. Anything Phase 0 drops (delay fields, poller, udev candidate, retroactive correction) routes to `docs/ideas/` with an index entry in `docs/ideas.md`.

#### Bench protocol

Capture terminals (packaged stack running; remote clients on a second device). All stamps wall-clock — correlate across terminals by time.

```sh
# T1 — portal D-Bus signals (per-message time= stamps)
dbus-monitor --session "path_namespace='/org/freedesktop/portal/desktop'"
# T2 — PipeWire graph events (ts via moreutils: nix shell nixpkgs#moreutils)
stdbuf -oL pw-mon | ts '%H:%M:%.S'
# T3 — tisvlay logs: lock/unlock, idle, dpms watcher edges
journalctl -f -o short-precise --user-unit tisvlay-session
journalctl -f -o short-precise -u tisvlay-daemon
# T4 — DPMS sysfs edges (set $p to the TV's connector)
p=/sys/class/drm/cardX-HDMI-A-Y/dpms; l=; while :; do v=$(cat $p); [ "$v" != "$l" ] && echo "$(date +%T.%3N) $v"; l=$v; sleep 0.1; done
# T5 — Sunshine /serverinfo poll (1 s)
while :; do echo "$(date +%T.%3N) $(curl -sm1 http://127.0.0.1:47989/serverinfo | grep -oE 'SUNSHINE_SERVER_(BUSY|FREE)')"; sleep 1; done
# T6 — udev input-device add/remove (Sunshine per-client candidate)
udevadm monitor --udev --subsystem-match=input
```

Optional hook-timing sample: give Sunshine a bench-only `global_prep_cmd` whose do/undo append `date +%T.%3N` to a file — measures hook latency vs T5 without wiring anything.

Scenarios — ≥3 cycles each, for each of krdp (RDP client), RustDesk, Sunshine+Moonlight (note which capture backend Sunshine logs at startup):

- **A locked**: screen locked → connect → unlock via remote → interact → disconnect. (Race: portal signal vs unlock in T3.)
- **B dark**: idle until DPMS off → connect → interact → disconnect. (Race: portal/node vs T4 dpms-on.)
- **C blip**: mid-session client reconnect (Moonlight quit+resume, RustDesk reconnect, RDP re-attach). (Flap: count T1/T2/T5 edges.)

False-positive probes (T2 only, once): OBS record start/stop, browser tab-share start/stop, taskbar-thumbnail hover, Overview toggle.

Fill the three decision rows from the traces: race-window sizes (earliest remote signal → unlock/dpms-on deltas), flap counts per blip, poller/hook latency.

- `keys` (subsystem-family shape, matching `user/profile/*` and `user/tray/*`): `user_remote_session(site)` → `tisvlay/<site>/user/remote/session` (cached pub on the user socket — the session owns that router, so full cached-pub hygiene applies: advanced pub, `sample_miss_detection` + `publisher_detection`, liveliness declared after serving); `remote_session(site)` → `tisvlay/<site>/remote/session` (**plain put** mirror on the system socket — no cache, no detection, no token; the session's system link is a connect-only peer and served entities do not propagate over it, per the consume-only-peer law); `user_remote_rpc(site, verb)` → `tisvlay/<site>/user/remote/rpc/<verb>` + `_wild` + `parse_` (own queryable family, tray-settings precedent — not folded into the profile rpc queryable).
- `payload`: `RemoteSessionState { sources: Vec<RemoteSource>, armed: Vec<Uuid> }`, `RemoteSource { kind: RemoteSourceKind, detail: String }`, `RemoteSourceKind { Manual, Sunshine, Logind, Portal, Screencast }`.

### 2. Profile schema (`crates/tisvlay-core/src/config.rs`)

Per-field cost is ~5 hand-edits, not 3: config `Option<T>` field, macro `fields:[]` entry, `Resolved*` field, its `Default` line, and the `resolved_json` projection in the session's `effective` rpc (`tisvlay-session/src/lib.rs:814`) — miss the last one and the GUI/CLI effective view silently omits the field.

User profile (`UserProfileConfig` :431, macro :472, `ResolvedUserProfile` + `Default` :497/:514) — 6 fields:
`on_remote_connect`, `on_remote_disconnect` (`Option<EventAction>`, default None), `remote_screencast_arms` (`Option<bool>`, default false), `while_remote_on_session_unlock`, `while_remote_on_lock_wake`, `while_remote_on_idle_resume` (override fields, default Inherit).

System profile (`SystemProfileConfig` :288, macro :341, `ResolvedSystemProfile` :370/:395) — 2 fields:
`while_remote_on_dpms_on`, `while_remote_on_wake` (override fields, default Inherit). No `while_remote_on_dpms_off` — overrides are wake-events-only. Never add `on_remote_*` events to the system schema.

**Override representation**: "unset = inherit" does not survive the macro's uniform `Option<T> → T` resolve (`Option<Option<EventAction>>` is a serde wart, and no `EventAction` value means Inherit — `none` means suppress). Use a dedicated override type with an explicit `Inherit` variant as the resolve default (wire token `inherit`, unset field = Inherit via serde default); it must compose with `EventAction`'s untagged bare-string/object duality — exact serde shape decided at implementation. GUI override dropdowns list `inherit` + the five action tokens.

Delay fields are **excluded from v1** (Phase 0-conditional; see Decision 6). Stock defaults keep every new field neutral — the feature is opt-in via profile editing (PR #34 precedent). No versioning/migration (absence defines v1).

### 3. Session service (`crates/tisvlay-session`)

- New `remote.rs` module: aggregator task owning source state; detectors report over an `mpsc`; recomputes `armed` on source or profile changes (profile watch like `idle_timeouts`, re-publish debounced); publishes the user-socket fact (cached) + system-socket mirror (plain put; re-put on daemon liveliness appearance); drives `SessionEvent::RemoteConnect`/`RemoteDisconnect` into `Core` (extend the enum at `lib.rs:48` + `tap_name()`; add match arms in `action_for` :467 mapping to the new resolved fields).
- **Seam (required by both main.rs and tests)**: extend `SessionStack` (`lib.rs:95`) and `serve()` with the detector-report sender — real detector tasks (spawned in `main.rs` like `watch_screensaver`/`idle_adapter`) and the test Rig both inject through it.
- `Core` tracks per-TV armed state (new map beside `screen_on`/`drift_at`); the while-remote override gate lives in `action_for`, keyed on `armed(tv)` — additive to the `automated` checks, replace-only semantics (Decision 3).
- Detector tasks:
  - **verb**: new queryable on `user_remote_rpc_wild` (own family + callback, tray-settings precedent) serving `set {source, active}` / `status`; inline replies.
  - **pipewire**: dedicated OS thread (pipewire-native `ThreadLoop` + `Core` + `Registry`; Context held alongside Core), registry `global`/`global_remove` + node-info listeners, predicate `media.class == "Stream/Output/Video" && node.name == "kwin_wayland"` (+ state) factored as a pure fn; reports the `Screencast` source; enumerates at startup (hydration). Keep the glue thin — it is the one CI-untestable strip and pulls against `cov_baseline_lines`.
  - **monitor**: second zbus connection → `become_monitor` (zbus 5.16 `fdo::MonitoringProxy`, confirmed) with match rules on the portal path namespace; parses ScreenCast/RemoteDesktop `CreateSession`/`Start`/`Closed` (pure parser fn over captured-message fixtures); sender → `get_connection_unix_process_id` on the **main** connection (monitor connections are receive-only) → `/proc/<pid>/exe` → built-in allow-list (`krdpserver`, `rustdesk`, `sunshine`) → `Portal` source; **InputCapture interface ignored unconditionally**; bootstrap tree-walk of `/org/freedesktop/portal/desktop/session/*` joins owner exe × video-node presence.
  - **sunshine** *(Phase 0-conditional)*: 3 s localhost poll of `:47989/serverinfo` gated on a cheap procfs presence check for the sunshine process (fine here — the session service runs unsandboxed in the user session, unlike the daemon's `ProcSubset` lesson); `SUNSHINE_SERVER_BUSY` → `Sunshine` source.
  - **logind**: system-bus zbus watch of `SessionNew`/`SessionRemoved` + `Remote` property for own-uid sessions → `Logind` source.

### 4. Daemon + world

- Daemon: bare `declare_subscriber` on the mirror key (greeter pattern, `crates/tisvlay-daemon/src/lib.rs:274`) → forward to the world.
- World (`crates/tisvlay-world`) — three net-new pieces: an `InEvent::RemoteSession { state }` variant (`events.rs:17` family), a `RemoteArmed(HashSet<Uuid>)` resource + its updater system, and a `resolve_while_remote_override` helper (separate from `profile_event_action` — override fields have no stock floor in the `SystemProfile` resource; unset = inherit is correct). Gate in `dispatch_system_events` (`systems.rs:1959`, beside `is_automated`): for armed TVs, resolve the override for `DpmsOn`/`Resume` (unset → normal, `none` → skip, action → replace). `Sleep`/`Shutdown`/`Startup`/`GreeterIdle`/`GreeterResume`/`DpmsOff` arms untouched. Publish the armed projection for clients (`publish_changed` chain, `systems.rs:58`) so the tray/GUI can indicate it.

### 5. SDK, CLI, tray

- SDK (`crates/tisvlay/src/lib.rs` + `mirror.rs`): `remote_session` slot in `UserMirror` + parse branch in `apply()` + `StateEvent::RemoteSessionChanged`; `SessionClient::remote_session_set/status` (RPC via `Caller` — matching-gated, no-reply retry).
- CLI (`crates/tisvlay-cli/src/main.rs`): `tisvlay remote-session start|stop|status [--source NAME]` — the documented Sunshine `global_prep_cmd` integration (`do` = start, `undo` = stop; fast, fire-and-forget so a slow hook can't stall Sunshine's launch).
- Tray (`crates/tisvlay-tray`): read-only "Remote session active" indicator item following the `managed_toggle` shape; quick-toggle deferred.

### 6. GUI (`crates/tisvlay-gui`) — honestly scoped: a multi-file feature, not table rows

- **New `FieldKind::Bool`** (`src/profiles.rs:73` — currently Enum/Number only, Bool explicitly reserved): extend `build_field_raw`, `read_*_field`/`set_*_field`, and add a `ProfileFieldRowBool` Slint component (Switch-based) + a new repeater branch (`ui/components/profiles_pane.slint:1007`).
- **Second field table** `WHILE_REMOTE_FIELDS` + second model/repeater rendering as the WHILE REMOTE ACTIVE card below FIELDS (uppercase mini-title convention), or a merged model with a card-break marker — implementer's choice, but the card visual is the approved design (storyboard below).
- **Divergence tests**: the existing per-table GUI validation (`profiles.rs:1986/2000`) must assert `USER_FIELDS ∪ WHILE_REMOTE_FIELDS == USER_PROFILE_FIELDS` (and the system-side union) or the new table silently drifts.
- Override dropdowns list `inherit` + action tokens; `remote_screencast_arms` renders as the Switch row (label "Screen capture arms").
- New strings via `@tr()` / `pgettext("profile field", …)`; run `just i18n-extract` and commit the `.pot` diff.
- Chosen pane (user profile; system profiles get the parallel override card for their wake rows):

```
USER PROFILE                                    [⧉] [↻]
┌ FIELDS ────────────────────────────────────────────┐
│ On session lock         [ screen_off       ▾ ]     │
│ …existing rows…                                    │
│ On idle resume          [ screen_on        ▾ ] ⟳   │
│ On remote connect       [ none             ▾ ]  NEW│
│ On remote disconnect    [ none             ▾ ]  NEW│
└────────────────────────────────────────────────────┘
WHILE REMOTE ACTIVE  ⓘ                            NEW
┌────────────────────────────────────────────────────┐
│ Screen capture arms       ⭘ off   (Switch)         │
│   · remote apps / hooks / logind always arm        │
│   · on: any screencast (OBS, tab-share) also arms  │
│ On session unlock       [ inherit          ▾ ]     │
│ On lock wake            [ inherit          ▾ ]     │
│ On idle resume          [ inherit          ▾ ]     │
│   inherit = behave as normal                       │
└────────────────────────────────────────────────────┘
```

(Delay spinboxes appear here only if Phase 0 ships them.)

### 7. Packaging & licensing

- `packaging/licensing/deny.toml`: `allow-git = ["https://gitlab.freedesktop.org/pipewire/pipewire-native-rs.git"]` with the rationale + revert condition commented **at the entry** (the single canonical home for the 0.1.4/0.1.5 facts; the Cargo.toml pin carries no duplicate rationale).
- **`packaging/nix/package.nix`: add `cargoLock.outputHashes` for the pipewire-native rev** — `importCargoLock` hard-requires it for git deps; without it `nix build`, `test.nix`, and the NixOS module (the primary deployment path) all fail. Remove on the crates.io revert.
- Update the two now-stale tripwire sentences asserting the workspace has no git deps: the `deny.toml` `[sources]` header comment and the `docs/licensing.md` pipeline paragraph.
- `just licenses` / cargo-about pass (three MIT sibling crates; NOTICE/THIRD-PARTY regenerate, no exceptions).

### 8. Docs

- **`docs/remote/detection.md`** (new; sibling to docs/watchers/): the user-facing setup + reference home — Sunshine `global_prep_cmd` JSON snippet, detector matrix (what covers what), false-positive list (lan-mouse, OBS with the switch on), poller opt-out, and the D-Bus note (BecomeMonitor rides the session bus, owner-allowed under both dbus-daemon and dbus-broker — no packaged policy needed).
- `docs/plans/README.md` active-plans entry (added with this plan's baseline commit).
- **On-merge migration map** (per README convention): architecture.md gains the two keys in the key-space table + a session-watchers paragraph; CLAUDE.md quirk candidates (InputCapture never arms; monitor connections are receive-only — resolve senders on the main connection; Sunshine is portal-invisible on Nvidia); Phase 0-dropped items + deferred list → `docs/ideas/<topic>.md` with index entries in `docs/ideas.md`; this plan file then deletes.
- Pre-merge sweep: no plan-phase/PR references or tombstones in code comments.

## Testing

- **Pure fns** (unit): arming computation (sources × screencast-arms policy), immediate-on/delayed-off edges, override resolution (inherit/replace/suppress semantics incl. the replace-only invariant), the PipeWire node predicate (struct fixtures), the portal-monitor message parser (captured-message fixtures, **with capture provenance — xdp version — recorded beside them**) — same factoring discipline as `idle_dispatch`/`lock_wake_policy`.
- **Session integration** (`crates/tisvlay-session/tests/session.rs` `Rig`): inject detector reports through the new `SessionStack` report-channel seam and `SessionEvent::RemoteConnect/Disconnect` via `client.send`; assert mock-TV actions, override suppression of idle-resume/unlock, fact + mirror key contents, re-put on daemon liveliness. Zenoh tests stay `#[tokio::test(flavor = "multi_thread")]`; waits use combined predicates in a single drain.
- **Daemon/world integration** (`crates/tisvlay-daemon/tests/`): mirror-key consumption → world gate honored for DpmsOn (greeter-test pattern); sleep path unaffected while armed; empty-armed-set invariant.
- **Wire/SDK** (`crates/tisvlay/tests/client.rs`): fact hydration from the user-socket cache, RPC verb round-trip via `Caller`.
- **Fakes**: fake logind exists (`tisvlay-test`); the portal monitor is covered by parser fixtures + channel injection; PipeWire has no fake in v1 — the pure predicate carries the logic, the thin glue is verified live (documented gap; candidate for docs/ideas/).

## Live verification (user-assisted, real hardware)

1. krdp session from another machine: fact arms (`portal:krdpserver` + `screencast`), unlock/idle-resume/DPMS-on suppressed per overrides, TV stays off; disconnect → disarm, `on_remote_disconnect` action fires.
2. RustDesk: same expectations via portal attribution.
3. Sunshine + Moonlight (NvFBC path): arms via the prep-cmd hook and/or poller; confirm Moonlight input trips ext-idle-notify unsuppressed *before* the feature and suppressed after.
4. False-positive probes: OBS recording and a browser tab-share with `remote_screencast_arms` off (no arming) and on (arms); lan-mouse present throughout (must never arm); Plasma thumbnails/Overview spot-check for transient `Stream/Output/Video` nodes.
5. `tisvlay remote-session start` / `stop` round-trip; fact transitions visible via the mirror on `tisvlay watch` (system socket) and via `--user-socket` for the user-socket fact; session-service restart mid-krdp-session re-arms (hydration); daemon restart mid-session re-arms (liveliness re-put).

## Refactor: while_remote as an action subfield (2026-07-11, post-prototype)

The prototype validated the gate end-to-end, and the parallel `while_remote_*` field family showed its cost: a hand-picked event subset (an editorial choice baked into the schema), ~5 edits + a GUI row per field, a second GUI card, and a dedicated override type with an `Inherit` sentinel. The generalization (user decision): **a per-event gate bool on the event action itself**, following the `restore_input` precedent —

```toml
[profile.body.on_session_unlock]
action = "screen_on"
restore_input = true
while_remote = false                        # this event dispatches nothing while the TV is armed
```

- **Semantics**: `while_remote: bool`, default `true` (the bare-token form implies it) — whether the event's action still fires while the TV is armed. `false` = suppressed. A pure gate: suppression only, so the review's replace-only safety invariant holds **by construction** (a gate cannot originate an action), and the flag is naturally inert on `action = "none"` rows. Honored uniformly, including the formerly exempt events (sleep/shutdown/startup — **this consciously reverses the review's never-gate rule for the per-event-explicit case**; docs recommend leaving sleep/shutdown ungated; greeter events are naturally inert under the no-session ⇒ empty-armed invariant).
- **Considered and rejected — action-enum subfield** (`while_remote = "screen_off"`, redirect-per-event): every real use case is binary, mid-session state-switching is already carried by `on_remote_connect`/`on_remote_disconnect`, and the enum costs a two-type serde layer plus a second GUI dropdown column. Revisit only if an input-switch action ever lands (no config compatibility, so bool → enum evolution stays cheap).
- **Types**: single-type change — the event-action table gains the optional bool. `on_remote_connect`/`on_remote_disconnect` keep the same type; dispatch ignores the flag on those two events and the GUI hides the toggle there (connect fires at the arming edge, so honoring its own gate would silently swallow its action; a shared type with one inert bool beats a second type — supersedes the earlier enum-motivated type-split call).
- **Merge granularity**: whole-value, as `restore_input` already behaves — setting an event on a custom profile replaces the entire table. Documented consequence: gating an event whose action came from the stock floor/default profile requires restating the base action (the GUI does this automatically since the row always shows the resolved action).
- **Deleted**: the 5 `while_remote_on_*` fields, `WhileRemoteOverride` + `InheritToken`, the world's `resolve_while_remote_override`/`while_remote_override_field` helpers and their exemption list, the GUI WHILE-REMOTE override card + union divergence tests. Dispatch reduces to `if armed && !action.while_remote { skip }` at both gates.
- **Unchanged**: everything below the resolve layer — the fact, mirror, armed recompute (incl. the `4607cc5` cold-start fix), detectors, wire keys, SDK, CLI, tray. `remote_screencast_arms` stays a profile field; the GUI's remote card shrinks to that switch (+ future bench-gated knobs).
- **GUI**: each event row gains a `restore_input`-style icon toggle (the existing `IconBtn` active pattern; tooltip "Suppressed during remote session"), shown only when the resolved action is not `none`.
- No config compatibility: the schema replaces the old fields outright (absence defines v1); existing dev configs are hand-migrated.

**Default flip + pane polish (user decisions, same day):** the gate's default inverts to **suppress-by-default** — serde default `while_remote = false` everywhere (a bare token or omitted field is gated; one rule), with a **direction-split floor**: off-going events carry an explicit `while_remote = true` in the stock resolved values — the power-offs `on_sleep`/`on_shutdown`/`on_reboot` (the machine going away still turns the TV off — the LGTVCompanion lesson survives as a floor value instead of dispatch machinery) plus the blanks `on_dpms_off`/`on_session_lock`/`on_idle_timeout` (a blank is always safe to let through, and an armed-but-ON TV must still darken for the empty room), while wake-ish events stay gated (startup so a boot while remoted stays dark, and the unlock/DPMS-on/resume mirrors). Armed now means "wake events quiet out of the box," and the per-event toggle expresses exceptions in either direction. GUI polish in the same pass: both row toggles (restore-input, gate) are uniformly visible on every action row — inert/dimmed where meaningless (restore on `none` rows, gate on the connect/disconnect rows) — killing the visibility-driven layout shift and making restore-input expressible on off-going actions; all row reset glyphs form one right-edge column aligned under the header reset-all, reusing the header's duplicate/reset spacing token, with the capture-arms switch row on the same grid; toggle accent semantics change from "value is on" to **"differs from the floor default"** (neutral at floor, accent when changed), matching the pane's locally-set visual language.

## Live QA findings (2026-07-11, dev stack)

- **Sunshine/NvFBC invisibility confirmed in the field**: with no hook configured, the fact showed zero sources during an actively BUSY stream — the `global_prep_cmd` hook is mandatory on this hardware (first data point for the poller decision). With the hook configured: `do` → source-applied measured at **7 ms**, arming **42 s** before the first injected input — the hook path wins the ordering race by construction.
- **Re-QA after the fix: PASS.** Full-stack restart → lock → Sunshine connect: armed 11 ms after the hook (both TVs, ahead of the user's `loginctl unlock-session` prep-cmd and all injected input); the unlock and four lock-wakes were suppressed per the user-profile overrides while an off-going lock→screen-off passed through (wake-only overrides working as designed); disarm on app quit restored normal wake dispatch immediately. Not yet field-exercised: the world-side `while_remote_on_dpms_on` gate (no DpmsOn occurred in-window) — covered by bench scenario B.
- **SSH-session false positive, fixed by graphical-type filter**: a plain SSH shell mints a `Remote=yes` logind session (live specimen: `Id=5, Service=sshd, Remote=yes, RemoteHost=192.168.1.74, Type=tty, Class=user`), falsifying the research claim that only xrdp-class stacks mint `Remote=yes` — remote *desktop* logins are `Type=x11`/`wayland`, SSH is `Type=tty`. Field symptom: an open SSH shell armed the remote-session fact at stack startup (hydration found the session, fired `RemoteConnect`), and the user's `on_remote_connect = screen_off` blanked the screen they were sitting at. Fixed by additionally requiring the session `Type` to be graphical (`x11`/`wayland`) in the logind detector's qualifying predicate.
- **Cold-start defect, fixed in `4607cc5`**: a full-stack restart raced the session's system link (first connect failed, resilient retry); the daemon-liveliness re-put mirrored a stale pre-hydration armed snapshot, and change-gating never retried the put dropped while the link was down — so the world gated nothing for the entire session despite correct sources and correct policy on both profile sides. Fix: recompute armed before the liveliness re-put and re-mirror unconditionally on that edge. The Rig cannot reproduce the exact field ordering (the advanced meta subscriber re-hydrates reliably in-harness; the fix's own tests pass even with the fix reverted), so re-QA must confirm on a full-stack restart with a pre-hydration arm that the system-socket mirror's `armed` list populates — if it stays empty, the suspect is the advanced meta subscriber's re-hydration after a first-connect failure (zenoh-level), not the session's recompute.

## Deferred (docs/ideas/ with index entries, on merge)

Tray quick-toggle + richer indicator; GUI editing of the portal allow-list; GNOME portability (mutter node names, first-class Mutter APIs); Steam Remote Play / Parsec detectors; configurable Sunshine endpoint; greeter-session remote interplay; PipeWire fake for integration tests; journald-tail detectors (rejected for brittleness — revisit only if a target app loses its signal); plus whatever Phase 0 drops (delay fields, poller, udev candidate, retroactive correction).
