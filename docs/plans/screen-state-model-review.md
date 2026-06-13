# Plan: screen-state-model review fixes (PR #83)

## Context

A max-effort multi-agent review of PR #83 (`feat/screen-state-model`) surfaced 34 verified findings, which collapse to the 15 distinct issues below. They cluster on the two newest, least-battle-tested mechanisms this PR adds — the per-TV `desired_screen` latch + push-driven `reconcile_screen`, and the `tisvlay-greeter` presence-assert — and several break guarantees the PR's own `docs/design.md` now asserts (notably A1, which makes the just-committed "Presence re-assert at the greeter" / "Cross-login handoff" paragraphs currently aspirational rather than true).

Work top-down by tier; commit per logical batch (a tier may be one commit or a few). This plan is the baseline tracker — decommission it together with [`screen-state-model.md`](screen-state-model.md) once the fixes merge, per [`README.md`](README.md).

Line numbers are as of the review (branch `feat/screen-state-model` at the time of writing); confirm each against the source while implementing.

## Tier A — must-fix (breaks a stated guarantee or reintroduces a prevented race)

### A1. Greeter presence-assert is dead in the default config
- Where: `crates/tisvlay-greeter/src/lib.rs:59` (the `if timeout_secs == 0 { return }` early-exit) sits *above* the startup presence-assert at `:82`; `crates/tisvlayd/src/tv_manager.rs:1043` (the `GreeterResume` fan-out gate).
- Bug: with the default config (every profile's `idle_timeout_seconds = 0`, so `GreeterIdleTimeoutSeconds == 0`) the greeter exits before sending `NotifyGreeterIdle(false)`, so the panel is never re-lit at the login screen after a logout-from-locked. A second gate compounds it: even when the assert *is* sent, the daemon's `GreeterResume` fan-out only acts on TVs with `idle_timeout > 0`, so a TV with greeter-idle disabled is skipped. The strand-dark problem is independent of whether proactive blanking is wanted.
- Fix: send the presence-assert *before* the `timeout == 0` return (assert, then exit if there is nothing to watch). Decouple `GreeterResume` from `idle_timeout` — gate it on `is_tv_automated` (you always want to restore presence; you only proactively *blank* for opted-in TVs). No `design.md` change needed — this makes the committed guarantee hold.

### A2. `reconcile_screen` has no `is_reconnecting` guard → reopens a competing connection during `power_on`
- Where: `crates/tisvlayd/src/tv_manager.rs:2982` (`reconcile_screen`); `apply_screen_*_raw → with_connection` calls `connect_tv` when `!is_connected()`.
- Bug: a late/stale subscription push during an in-flight `power_on`/reconnect spawns a reconcile that opens a *second* connection — the exact wake-race the direct `screen_on`/`screen_off` defer was added to prevent.
- Fix: gate `reconcile_screen` on `!is_reconnecting` (mirror the direct path); a deferred intent still lands via the reconnect-complete state push.

### A3. Power transitions never reset `desired_screen`
- Where: `crates/tisvlayd/src/tv_manager.rs:2251` (`power_on`) / the `power_off` path; `reconcile_screen` at `:2982`.
- Bug: `power_on`/`power_off` leave the latch untouched, so (a) a TV just powered **on** is immediately re-blanked by reconcile if an old `screen_off` latch persists, and (b) a TV powered **off** by the remote or a failed daemon op gets an unsolicited `turnOnScreen` from reconcile re-asserting `desired = Some(true)`.
- Fix: clear/refresh the latch on power transitions, and add a powered-off / disconnected guard to reconcile. (Pure sticky-**off**-until-presence is the intended contract per the parent plan — the bug is the power-transition and re-light facets, not sticky-off.)

### A4. Supervisor never retries a failed watch start nor detects a dead watch thread
- Where: `crates/tisvlay-session/src/lib.rs:396` / `:410` (`idle_watch_supervisor`).
- Bug: the loop only re-arms when the timeout *value* changes, so a transient Wayland-unavailable at session start, or a mid-session compositor restart, silently kills **both** lock-wake and in-session idle until the user happens to edit the timeout.
- Fix: retry on `start()` failure and detect watch-thread death (bounded retry / liveness signal), independent of a value change. Coordinate with C11 (the `watch` rewrite keeps the same retry gap unless this is addressed) and the manual `reconnecting` bool robustness note (#34, below).

## Tier B — should-fix correctness

### B5. `reconcile_screen` skips the `is_tv_automated` gate
- Where: `crates/tisvlayd/src/tv_manager.rs:2982`.
- Bug: every direct dispatch path (`execute_system_event_for_tv`, DPMS) checks `is_tv_automated` first; reconcile does not, so it drives screen commands at TVs the user never automated. (Implement alongside A2/A3 — same function.)
- Fix: gate reconcile on `is_tv_automated`.

### B6. Reconcile treats `apply_*_raw` `Ok(false)` as success
- Where: `crates/tisvlayd/src/tv_manager.rs:3008`.
- Bug: `Ok(false)` means the verify budget was exhausted (panel did not reach target); reconcile only logs `Err`, so a TV that persistently refuses re-runs the *entire* verify-retry loop (resending SSAP) on every subsequent push, including unrelated volume/input churn.
- Fix: on `Ok(false)`, stop re-attempting until a screen-relevant change (back off / mark and skip).

### B7. Lock-wake latch only clears on an idle/resume edge while unlocked
- Where: `crates/tisvlay-session/src/wayland_idle.rs:116` (`lock_wake_policy`); the swap reset at `:65`.
- Bug: `lock_wake_sent` is reset only when an `Idled`/`Resumed` edge is observed while unlocked. Since the transport now forwards only idle/resume edges (not every wayland event), a rapid unlock-then-relock with no intervening >200 ms idle gap leaves the latch stuck `true`, suppressing the next `LockWake` (the sub-100 ms snap-back silently doesn't fire). Separately (B13), a supervisor handle swap starts the new policy task with `lock_wake_sent = false`, so re-registering mid-lock-cycle can fire a *duplicate* `LockWake`.
- Fix: clear the latch on the lock→unlock *state transition* itself (driven by `is_locked`), not on an idle edge; carry the latch across a watch swap (or derive it from lock state so the swap is stateless).

### B8. `latest_wins_off_then_on_while_reconnecting` passes vacuously
- Where: `crates/tisvlayd/tests/profile_dispatch.rs:662`.
- Bug: after the reconnect the fresh `MockConnection` re-initializes to the default `Active` state (discarding the injected state), so the panel is already on when reconcile runs and the final `assert!(tv.is_screen_on)` passes *even if reconcile never applies the latched 'on'*. It only catches a latch-ends-off inversion, not a reconcile that fails to act on the latest intent — i.e. it does not actually test the headline latest-wins behavior.
- Fix: inject a non-`Active` post-reconnect state so the assert exercises reconcile applying the latest intent.

### B9. Reconcile read→apply TOCTOU
- Where: `crates/tisvlayd/src/tv_manager.rs:2988`.
- Bug: reconcile reads `(desired_screen, last_known_power_state)` under the per-TV lock, drops it, then runs `apply_screen_*_raw`. A concurrent direct `screen_on`/`screen_off` that flips `desired_screen` in the gap makes reconcile drive the panel the wrong way — no generation/latest-wins guard (cf. the GUI's `WizardGen`).
- Fix: re-check `desired_screen` under the lock at apply time, or carry a generation counter.

## Tier C — churn / altitude

### C10. Reconcile spawned on EVERY `SubscriptionUpdate`, not just power-state
- Where: `crates/tisvlayd/src/state_forwarder.rs:101`.
- Bug: volume/mute/input/backlight/app pushes each `tokio::spawn` a fire-and-forget reconcile; no coalescing. The *"serializes on the per-TV lock so concurrent spawns can't storm"* comment is misleading — reconcile releases the lock before `apply`, so it neither bounds the queue nor stops a stale reconcile from acting. The parent plan already flags this as known "reconcile spawn churn".
- Fix: gate the spawn on an actual `power_state` change (or coalesce); correct the comment.

### C11. Supervisor polls every 2s for a rarely-changing setting
- Where: `crates/tisvlay-session/src/lib.rs:374` / `:399`.
- Bug: a perpetual 2 s wakeup plus up-to-2 s apply latency for a value that only changes on GUI / `ReloadConfig` / SIGHUP — all of which already hold the write lock and know it changed.
- Fix: convert to `tokio::sync::watch`. `SessionState` owns a `watch::Sender<Option<u32>>` (effective idle-timeout ms; `None` = off); make `profiles` private and route mutation through a method that recomputes + `send_if_modified`s, so the publish can't be forgotten. The supervisor takes a `watch::Receiver` and parks on `changed().await` — zero idle wakeups, zero latency, and a free clean-shutdown break when the senders drop. A4's retry is still required (a failed `start()` is orthogonal to value change).

### C12. The two "deliberate parallel" idle resolvers are out of sync
- Where: `crates/tisvlayd/src/tv_manager.rs:1064` (`greeter_idle_timeout_seconds`) vs `crates/tisvlay-session/src/lib.rs:184` (`effective_idle_timeout`).
- Bug: the greeter *clock* gates on `idle_timeout > 0 && on_idle_timeout != None`, but the resume fan-out and the session `effective_idle_timeout` gate on `idle_timeout > 0` only — despite the comments instructing to "keep in sync".
- Fix: align the gates; the resume-vs-idle asymmetry decision from A1 settles which side moves.

### C13. Folded into B7
The supervisor handle swap resetting `lock_wake_sent` (`wayland_idle.rs:65`) is fixed together with the lock-wake latch semantics in B7.

## Tier D — cleanup

### D14. `WatchHandle::stop(self)` is dead public API
- Where: `crates/tisvlay-core/src/wayland.rs:165`.
- Bug: empty body, zero callers; both consumers stop the watch by dropping the handle.
- Fix: delete it (Drop is the contract), or wire it up as the documented stop path. Re-run `cargo workspace-unused-pub` to confirm no other orphans in the new surface.

### D15. `verify_config` refactor only half-applied + adjacent duplication
- Where: `crates/tisvlayd/src/tv_manager.rs:2597` (`screen_on`) / `:2770` (`screen_off`) inline a byte-identical copy of `verify_config` (`:3025`) instead of calling it; `apply_screen_on_raw` (`:2916`) / `apply_screen_off_raw` (`:2949`) are copy-paste twins; `screen_on`/`screen_off` acquire `get_tv` + the per-TV lock twice (once to set the latch, once via `is_reconnecting`) at `:2581`.
- Fix: call `verify_config`; reduce the `_raw` twin duplication (helper/macro differing only in the connection call + predicate + log string); fold the double lock-acquire where it stays clean.

## By-design / folded (not bugs to fix)

- **Sticky screen-off until presence** (review #11/#23/#28): the intended contract per the parent plan (`screen-state-model.md` lines 22–23, 31, 43, 132) — the panel stays OLED-off until real host presence returns. The genuinely-buggy facets are captured in A3 (power transitions / re-light), not "sticky-off is wrong". A latch with no expiry / no clear-on-external-observation is a deliberately-scoped boundary ("one display, one owner, the TV *is* the desktop"); revisit only if a user reports it.
- **Greeter socket lexicographic sort** (review #25, `greeter/lib.rs:210`): `wayland_sockets()` sorts names lexicographically and takes the first, so with `WAYLAND_DISPLAY` unset and multiple `wayland-N` sockets, `wayland-10` sorts before `wayland-2`. Narrow (greeter normally has `WAYLAND_DISPLAY` set, single socket); fix opportunistically with D-tier by numeric-suffix sort if touched.
- **Manual `reconnecting` bool vs RAII guard** (review #34, `tv_manager.rs:1246`): the defer gate two screen consumers depend on rests on a hand-managed bool with a new set-true site in `on_startup` whose clear could be skipped on an early `continue`/panic. Robustness/altitude; consider an RAII guard while doing A2/A4. Not a confirmed live bug.

## Verification

- `just check` green after each tier (fmt + build + clippy + nextest + doc).
- New/updated tests:
  - A1: greeter `run()` sends the presence-assert when `GreeterIdleTimeoutSeconds == 0`; daemon `GreeterResume` lights an automated TV with `idle_timeout == 0`.
  - A2/A3/B5: a stale push during reconnect does not open a second connection; `power_on` after a `screen_off` latch is not re-blanked; a `power_off` is not re-lit by reconcile; reconcile no-ops on a non-automated TV.
  - B6: a TV stuck refusing the target does not re-run verify-retry on an unrelated push.
  - B7: rapid unlock→relock still fires `LockWake`; a watch swap does not duplicate `LockWake`.
  - B8: rewrite to actually fail if reconcile skips the latched 'on'.
  - C10: a non-power push does not spawn an apply.
  - C11: a timeout edit re-registers without the 2 s poll (event-driven), clean teardown on shutdown.
- Hardware re-test (LG OLED, KDE Plasma Wayland): logout-from-locked with greeter-idle **disabled** still lights the login screen (A1); remote power-off is not re-lit (A3); a GUI idle-timeout edit applies instantly (C11).
