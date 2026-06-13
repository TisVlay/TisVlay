# Plan: DPMS-mirror default flip + greeter-idle OLED protection

## Context

On this Nvidia/KDE workstation the TV is the desktop display. Two gaps in OLED
burn-in protection at the **login greeter** (plasma-login-manager), established
from the KDE source and confirmed by a boot-log hardware test (2026-06-12):

1. **Esc-at-greeter isn't OLED-clean.** The greeter only blanks on a manual Esc
   (`greetereventfilter.cpp` → `KScreen::Dpms::Off`); that DPMS-off cuts the HDMI
   signal but can leave an LG OLED showing a lit "No Signal" banner. The daemon's
   DPMS mirror (`tisvlayd/src/dpms.rs`) already sees the connector go blanked, but
   `on_dpms_off`/`on_dpms_on` default to `SessionAction::None`
   (`crates/tisvlay-core/src/config.rs:352-353`), so it does nothing.
2. **Unattended greeter never blanks at all.** Boot → idle → zero DPMS
   transitions for 2+ min; PowerDevil only starts *after* login
   (`PartOf=graphical-session.target`); nothing protects the panel.

### Why a daemon-side blind timer can't do it; why a greeter-session client can
A daemon timer could SSAP `turnOffScreen` but has no pre-login signal for a
greeter keypress, so it couldn't know when to restore — black, signal-less panel.
Fixed by subscribing to the greeter compositor's `ext-idle-notify-v1` (delivers
both `idled` and `resumed`). Verified: the greeter's kwin exposes it
unconditionally (`kwin/src/wayland_server.cpp:416`); tisvlay already speaks it
(`tisvlay-session/src/wayland_idle.rs`). But the greeter runs under a systemd
**user** manager for the `plasmalogin` sysuser, with its wayland socket at
`/run/user/<plasmalogin-uid>/wayland-N` (mode `0700`). `tisvlayd` runs as the
unprivileged `tisvlay` user and cannot open it. So the watcher must run **inside
the greeter session as `plasmalogin`**, calling the daemon over the system bus —
structurally identical to how `tisvlay-session` serves the logged-in user.

## Decisions locked
- **Flip** `on_dpms_off`/`on_dpms_on` defaults to `ScreenOff`/`ScreenOn` (plain
  global flip; logged-in double-dispatch is redundant-but-safe, per Q1).
- **Build the greeter idle client**, opt-in via `idle_timeout_seconds`.
- **Generic client, plasma-login packaging only for v1** (`ext-idle-notify-v1`
  is standard; `connect_to_env()` finds the session compositor). Future greeters
  are packaging-only additions.
- **Shared wayland code goes in `tisvlay-core` behind a `wayland` feature**, NOT
  a new crate (317-line module, two in-workspace consumers — doesn't meet the
  repo's crate-granularity bar; revised after review). The greeter binary
  (`tisvlay-greeter`) *is* its own crate (distinct principal/unit/profile model,
  like `tisvlay-session`/`tisvlay-tray`).
- Frame `tisvlay-greeter` as the **first user-context per-DE adapter**, answering
  the open question in `docs/ideas/event-driven-power-management.md:40` ("whether
  system-bus tisvlayd can hold a Wayland connection" → no; a user-context helper
  does).

---

## PR 1 — Flip the DPMS-mirror defaults (small, ships first)

- `config.rs:352-353` (`ResolvedSystemProfile::default`): `on_dpms_off:
  ScreenOff`, `on_dpms_on: ScreenOn`.
- Fix tests asserting the old `None` defaults:
  - `crates/tisvlayd/tests/manager1_inherit_fallthrough.rs:141-142` (asserts
    `Some(None)` for both — **named explicitly**, not just "grep").
  - GUI fixture baseline `crates/tisvlay-gui/src/profiles.rs:1532-1533` (update
    for fidelity).
- Docs: `.claude/CLAUDE.md` DPMS quirk note (the "none by default…double-dispatch"
  rationale → new default; note logged-in double-dispatch is redundant-but-safe:
  ScreenOff idempotent, `is_reconnecting` guard at `dpms.rs:138` covers
  ScreenOn/WoL); `docs/design.md` TOML schema comment (`:511-512`) + Event→Action
  rows (`:649-650`).

Independent of PR 2. Esc-at-greeter win immediately. The greeter-idle path (PR 2)
uses SSAP, leaves the connector `dpms=On`, so it doesn't drive this mirror; the
Esc path keeps using it. **Wording correction (review):** on the Esc-then-resume
sequence both the mirror (`DpmsOn`) and the greeter client (`Resumed`) dispatch
`ScreenOn` — *double-dispatch, idempotent via the screen_on/off self-verify*, not
"no interaction." Also note: if a TV has drifted to `Standby`, `screen_off`'s
wake-preflight briefly powers it fully on before blanking (a momentary
full-brightness flash) — documented side-effect (`tv_manager.rs:2646-2649`).

---

## PR 2 — Greeter idle client (`tisvlay-greeter`), one reviewable unit

Mirrors the prior DPMS PR (`071df59`): watcher + system-profile fields + daemon +
GUI + docs land together so every line has its in-tree consumer. Internal commit
order below; **no "Phase N" strings in commit messages/comments** (describe the
change).

### a. Shared wayland idle transport → `tisvlay-core` `wayland` feature (refactor, no behavior change)
`wayland_idle.rs` imports exactly one internal symbol
(`crate::session_monitor::SessionEvent`, `:25`); all else is external → no cycle.
- New `tisvlay-core` module (feature `wayland`, pulling `wayland-client`,
  `wayland-protocols` — already in `[workspace.dependencies]` with `staging`).
  Port `WaylandState`, the `Dispatch` impls, the `blocking_dispatch` loop, and
  `WaylandIdleError`. Expose `pub enum IdleTransition { Idled, Resumed }` and
  `pub fn start_idle_watch(timeout_ms: u32, tx: UnboundedSender<IdleTransition>)
  -> Result<(), WaylandIdleError>` (timeout parameterized; **no
  `IdleWatchHandle`** — the thread self-terminates on disconnect, matching the
  existing detached shape).
- `tisvlay-session` enables `tisvlay-core/wayland`; `wayland_idle.rs` becomes a
  thin adapter: `start_idle_watch(200, raw_tx)` then translate
  `Resumed`→`SessionEvent::GreeterWake` under the existing
  `is_locked`/`greeter_wake_sent` policy. Public
  `start_wayland_idle_monitor(event_tx, is_locked)` signature unchanged
  (`session/main.rs:183` untouched). **Move the load-bearing 200 ms rationale
  comment (`wayland_idle.rs:27-34`) to the call site** so it isn't lost.

### b. System-profile idle fields (config; mirror the user profile)
The greeter is governed by the **system** profile, which lacks idle fields; the
user profile has the model (`config.rs:390-400`, defaults `443-453`).
- `config.rs`: add `idle_timeout_seconds: Option<u64>`, `on_idle_timeout:
  Option<SessionAction>`, `on_idle_resume: Option<SessionAction>` to
  `SystemProfileConfig` + the `profile_plumbing!` `fields:` list (auto-derives
  merge/resolve/from_config/`From<&Body>`/`SYSTEM_PROFILE_FIELDS`). Hand-edit the
  three macro-exempt sites (`config.rs:170-172` documents these stay manual):
  `ResolvedSystemProfile` struct + its `Default` (**core default
  `idle_timeout_seconds: 0` = off**, `on_idle_timeout: ScreenOff`,
  `on_idle_resume: ScreenOn`) and `SystemProfileBody`.
- GUI (`crates/tisvlay-gui/src/profiles.rs`): add three `FieldSpec`s to
  `SYSTEM_FIELDS` — **one Number** (`idle_timeout_seconds`, copy the user Number
  spec) + **two Enum** (SessionAction). Add **non-uniform** arms to
  `read_system_field` (`:402`, the Number arm maps `body.idle_timeout_seconds.map(|v|
  v.to_string())`) and `set_system_field` (`:1119`, Number arm needs
  `number_patch`, the system side has none today). `field_label` arms optional —
  strings already exist (`:242-244`; `.pot:88-96`) so **no new i18n**. The
  existing parity test `system_field_table_matches_core_schema` (`:1727-1742`)
  catches a missing `SYSTEM_FIELDS` entry — no new test needed for that.

### c. Daemon dispatch + Manager1 D-Bus surface
- `crates/tisvlayd/src/tv_manager.rs`: add `SystemEvent::GreeterIdle`/`GreeterResume`
  (`:548`, forces the exhaustive `get_system_action` match `:1008` → map to
  `on_idle_timeout`/`on_idle_resume`); extend the `SystemProfileBody` projection
  in `get_effective_system_profile` (`:1380`, exhaustive literal — E0063 if
  missed); add `pub async fn greeter_idle_timeout_seconds(&self) -> u64` — a **new
  resolver loop** (iterate `list_automated_tv_ids`, `resolve_profile_by_uuid` per
  TV, min non-zero), *not* a mirror of the session's in-memory
  `effective_idle_timeout` (the daemon has no pre-resolved system-profile map).
- `crates/tisvlayd/src/dbus/manager.rs`: add Manager1 methods
  `notify_greeter_idle(idle: bool)` (→ `execute_system_event(GreeterIdle/Resume,
  "greeter")`, fans out per-TV via the existing
  `is_tv_automated`+`get_system_action`+`apply_action` path) and
  `greeter_idle_timeout_seconds() -> u64`.
- `crates/tisvlay-core/src/dbus_client.rs`: add both to the **`Manager1`** proxy
  trait + `DaemonClient` wrappers — alongside `reload_config` /
  `list_system_profiles` / `get_effective_system_profile` (`~:273`), **not** near
  the `Tv1.Screen` `screen_on`/`screen_off` block (those are per-TV; review fix).

### d. The `tisvlay-greeter` binary
- New `crates/tisvlay-greeter/` (bin+lib like `tisvlay-session`; deps
  `tisvlay-core` [`dbus`,`logging`,`wayland`], `tokio`, `tracing`, `anyhow`,
  `clap`). Add to `members` **and** `default-members` (shipping binary).
- `lib.rs` loop (testable): fetch `greeter_idle_timeout_seconds()` (brief retry if
  daemon starting); `0` → log "greeter idle disabled" + exit; else
  `start_idle_watch(t*1000, tx)`; map `Idled`→`notify_greeter_idle(true)`,
  `Resumed`→`notify_greeter_idle(false)`; **track last-sent edge, skip redundant
  repeats**; exit `Ok(())` on watch disconnect (greeter ended). `--user` flag →
  `DaemonClient::new_session()` for dev, else `new_system()`.

### e. Packaging (NixOS-first; deb noted)
- `packaging/systemd/tisvlay-greeter.service` — a **user** unit mirroring the
  sibling greeter units `plasma-login.service`/`plasma-wallpaper.service`
  (the env-inheritance precedent, **not** the post-login session unit):
  `After=`/`Requires=plasma-login-kwin_wayland.service`,
  `PartOf=`/`BindsTo=plasma-login-wayland.target`, `Type=simple`,
  `ExecStart=…/tisvlay-greeter`, `Restart=on-failure`. No `Environment=` —
  `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR` come from the plasmalogin user manager.
- `packaging/nix/module.nix` — opt-in `greeter.enable`; when set:
  `systemd.user.services.tisvlay-greeter` + a `systemd.user.targets."plasma-login-wayland"`
  drop-in (`overrideStrategy = "asDropin"`, `wants = ["tisvlay-greeter.service"]`
  — **verify the attribute resolves on the pinned nixpkgs**) + an explicit
  `<policy user="plasmalogin">` send/receive grant (defense-in-depth;
  `context="default"` already permits it — verified). **`greeter.enable` sets the
  system profile's `idle_timeout_seconds` to a sane default (e.g. 600s,
  configurable)** so opting in actually protects (see "Opt-in default" below).
  Soft `warnings` (not assert) if
  `services.displayManager.plasma-login-manager.enable` is false.
- deb/nfpm (`crates/tisvlayd/Cargo.toml` `[package.metadata.nfpm]`): add binary +
  user unit + target drop-in to `contents`; add the `plasmalogin` policy block to
  `org.tisvlay.Daemon.conf`. Distro greeter-user differs (sddm); `context="default"`
  covers it — keep v1 NixOS-only, flag in `packaging/README.md`.

### f. Cross-login handoff (CRITICAL — the review-blocking fix)
Do **not** rely on a "user-session replay" — verified it does **not** fire
`ScreenOn` at a fresh login (`session/main.rs:118-161` is gated on
`!daemon_available`; `session_monitor.rs:127-150` only emits `SessionUnlocked` on
a lock→unlock signal; `logind.rs:304-316` `SessionNew` is a no-op). Correct model:
- **Interactive login (common):** the keypress that wakes the greeter to type the
  password is itself an `ext-idle` `Resumed` edge → client sends
  `notify_greeter_idle(false)` → `ScreenOn` *before* login completes (the in-flight
  daemon→TV command survives the greeter exiting). Covered by (d)'s loop.
- **Zero-input login (autologin / programmatic):** no `Resumed` edge → panel
  left off. **Accepted limitation, not patched** — see the Decision below.

## Decision (after on-hardware confirmation): drop `on_login`, remove `on_no_session`

Once the greeter idle/resume path was confirmed working on real hardware, we
revisited the event model. Two distinct pairs exist, driven by different signals:

- **Greeter-presence pair** (`on_idle_timeout` / `on_idle_resume`): driven by
  *user presence* at the login screen (compositor `ext-idle-notify`).
  Self-consistent — blanks when you walk away, restores on any input. This is the
  correct and complete OLED-protection mechanism at the greeter.
- **Session-lifecycle pair** (`on_no_session` / hypothetical `on_login`): driven
  by *logout/login transitions*, fires immediately. A *power-policy* tool (e.g.
  power the TV off between sessions), not a screen tool.

Conclusions:

- **`on_login`: not built.** Interactive logins are already covered by the greeter
  resume edge (login requires input → resume → `screen_on` before login completes).
  `on_login` would only serve a zero-input autologin after an idle-blank, or be the
  symmetric partner to a power-action `on_no_session` — neither is in use. YAGNI.
- **`on_no_session`: removed.** Superseded by the greeter idle pair for the screen
  use case, and actively *wrong* for screens (`on_no_session = screen_off` blanks
  via SSAP on logout, but the compositor never sees it, so no resume restores it →
  stranded black login screen). Its only coherent use was a `power_off` kiosk
  policy, which itself needs `on_login` — neither wanted. The logind session
  tracking (`check_active_sessions`, `SessionNew`/`SessionRemoved`) existed solely
  to fire it and is deleted with it. No backwards-compat requirement: the field,
  the `SystemEvent::NoSession` variant, and the tracking are removed outright (no
  deprecation shim).

---

## Compile-forced edit sites (consolidated from impl review — easy to miss)
- `config.rs:2667` (`system_profile_body_round_trip_full`) and `:2728`
  (`system_profile_body_from_config_preserves_options`) — full literals, **E0063**.
- `tisvlayd/tests/manager1_inherit_fallthrough.rs:141-142` — old DPMS `None`
  assertions (PR 1).
- `tv_manager.rs:1380` projection literal — E0063.
- GUI `profiles.rs`: `baseline_system()` (`:1524`) — E0063; ~15 exhaustive
  `SystemProfileBody { … }` literals across the file each gain three fields;
  `read_system_field`/`set_system_field` arms are **not** compile-forced
  (fall-through) — rely on the parity test + add the arms.
- `state.rs` uses `..Default::default()` → safe.
- Root `Cargo.toml` `members` + `default-members` (greeter bin).

## Opt-in default (resolved after review)
Core library default stays `idle_timeout_seconds: 0` (matches the user-profile
precedent; no surprise blanking on upgrade — the DPMS mirror flip is the only
default-on change, and it's reactive/safe). But because the whole greeter path is
gated behind the Nix `greeter.enable` option, **that option supplies a non-zero
default (600s, configurable)** — so enabling greeter protection actually protects,
closing the discoverability gap the design review flagged, without a global
default-on.

## Multi-TV caveat (document, don't over-claim)
One compositor = one idle clock, but actions are per-TV. `greeter_idle_timeout_seconds`
uses **min non-zero** across automated TVs — *pragmatic, not "correct"*: with
heterogeneous timeouts a longer-timeout TV's `on_idle_timeout` fires early.
Document in the same words as the session's existing caveat (`lib.rs:175-178`).
The v1 target (one TV = the desktop) is exact.

## Verification
- `just check` (fmt + build + clippy + nextest + doc).
- Unit: config resolve/merge/body round-trip for the new fields (join
  `config.rs:1817+`); `get_system_action(GreeterIdle/Resume)`;
  `greeter_idle_timeout_seconds` min-non-zero; greeter `lib.rs` loop with a mocked
  `DaemonClient` + a fed `IdleTransition` stream (keeps the binary's testable
  surface up — see ratchet note).
- Integration (`tisvlay-harness` over `PrivateBus` + mock): system profile with
  `idle_timeout_seconds>0`, `on_idle_timeout=ScreenOff`; `Manager1.NotifyGreeterIdle(true)`
  → mock recorded `turnOffScreen`; `(false)` → `turnOnScreen`;
  `GreeterIdleTimeoutSeconds` returns configured / `0` when off.
- Hardware: PR 1 — Esc → true OLED panel-off (not "No Signal"), keypress →
  restored. PR 2 — set the timeout, walk away → blank after timeout → mouse →
  restored → log in → panel on (interactive resume edge). Re-pull boot logs.

## Risks / coverage / docs
- **Coverage ratchet** (`just cov-ratchet`): the new binary's wayland half is
  untestable without a fake compositor (as `wayland_idle.rs` is today) — keep the
  binary thin, unit-test the decision loop via a mocked client, and budget a
  deliberate ratchet-baseline adjustment in the PR.
- **D-Bus skew:** `SystemProfileBody` is `deny_unknown_fields`; newer GUI vs older
  daemon rejected — fine, they ship together.
- **Docs (land in PR 2, mirror `071df59`):** `docs/design.md` system TOML schema
  (`:497-513`), `SystemProfileBody` struct (`:375-379`), Event→Action table
  (`:638-650`), per-DE section (`:693-701`, `:955-957`) gains a "greeter idle
  client" subsection framed as the first per-DE adapter; update
  `docs/ideas/event-driven-power-management.md:40`. **Retire this plan file into a
  design.md subsection** per `docs/plans/README.md` convention.

## Status: implemented (branch `feat/power-default`)

Shipped as a commit series: the DPMS-mirror default flip; the
`tisvlay-core::wayland` transport extraction; the `tisvlay-greeter` client;
system-profile idle fields + daemon dispatch + `Manager1.NotifyGreeterIdle` /
`GreeterIdleTimeoutSeconds` + GUI rows; and packaging (systemd user unit +
NixOS `greeter.enable` + nfpm). Confirmed working on hardware
(plasma-login-manager greeter: 30 s idle → blank, activity → restore).

Deviations from the plan above, learned on hardware:

- **The connect path ended up simpler, not more complex.** A socket-discovery
  / `from_socket` detour was added while chasing a connection failure, then
  reverted to plain `connect_to_env` once the real cause surfaced: the
  `tisvlay-greeter` binary lacked `libwayland-client.so` on its RUNPATH
  (package.nix's `postFixup` rpath splice covered only gui/session). Fixed in
  packaging; the greeter connects with a lean startup retry.
- **`on_login` not built, `on_no_session` removed** — see the Decision section
  above. The greeter idle/resume pair is the complete OLED mechanism;
  interactive logins are covered by the resume edge.
- The login-time safety-net testing in Verification is moot (no `on_login`).

Retire this file into `docs/design.md` when the PR merges, per
`docs/plans/README.md` — the durable content already lives in the design.md
"Greeter idle client" subsection.
