# Plan: Coalesce deferred screen actions across reconnects

## Context

Hardware testing of the greeter idle client surfaced a cold-boot interaction. At
boot the TV is fully off; `on_startup = power_on` WoLs it, and an LG cold-boot
takes ~85 s to reach Active. A short greeter `idle_timeout_seconds` (30 s) fires
*during* that boot: the greeter `screen_off` runs its "pre-flight: powering on
first" branch (because the TV isn't Active yet), tangling with the in-flight
`on_startup` power-on, and the net result is a screen that stays on.

When we considered deferring the greeter `screen_off` while the TV is
reconnecting, the user identified the real hazard: **a naive defer-and-replay
would fire a stale `screen_off` after the user has logged in** (when
`on_session_unlock` / the resume edge wants the screen *on*). Defer-and-drop is
also wrong — the greeter idle is a single `ext-idle` edge, so a dropped edge is
never recovered and an away user loses OLED protection.

The correct model is to **coalesce to a per-TV desired screen state**: while the
TV can't accept a screen change (reconnecting / not yet Active), record the
*latest* intent and apply only that once the TV is ready; a later intent
supersedes an earlier one. Because every screen source funnels through one sink,
a login's `screen_on` naturally supersedes a deferred greeter `screen_off` — no
re-added login tracking needed (we removed `on_no_session` for good reasons).

## Current code (from exploration)

- **Sink:** `ActionExecutor` (`tisvlay-core/src/action.rs`) → `TvManager::screen_on`
  / `screen_off` (`tv_manager.rs:2546` / `:2703`). All paths reach here: greeter
  `Manager1.NotifyGreeterIdle` → `execute_system_event`; session `Tv1.Screen.SetScreen`;
  direct GUI/CLI `SetScreen`; `on_startup`/`on_wake`/DPMS via `execute_system_event*`.
- **Pre-flight:** `needs_wake_preflight` (`power_state.rs:102`) is true for any
  non-`Active`/`ScreenSaver`/`ScreenOff` state; `screen_off`/`screen_on` then call
  `power_on` first. Neither checks `reconnecting`.
- **`reconnecting` flag:** set true at the top of `power_on` (`:2239`) and
  `reconnect` (`:1970`); cleared on success (`:2337`, `:1988`) and failure paths,
  where `ConnectionChanged(true)` is also emitted. **This success point is the
  reconcile hook.**
- **Power state:** `last_known_power_state` (`:339`) updated by subscription push
  (`:632`) / `refresh_power_state` (`:2502`); `power_state_tx` watch drives
  `await_power_state`.
- Existing primitives: `Coalescer` (slider-drag merge), `Reconciler` (value-ramp
  intent-vs-observation), `RefreshTrigger` — none fit directly; this needs its own
  small mechanism.

## Design (recommended): per-TV screen intent + generation, reconciled on ready

Add to `ManagedTv`:
- `screen_intent: Option<bool>` — the latest desired screen state (`Some(true)` =
  on, `Some(false)` = off, `None` = nothing pending), or fold into a small struct.
- `screen_intent_gen: u64` — bumped on every screen request; lets an in-flight
  action detect it was superseded.

`screen_on(tv)` / `screen_off(tv)` become:
1. `gen = ++screen_intent_gen; screen_intent = Some(target)`.
2. **If `reconnecting`:** return `Ok(())` (deferred — the reconcile hook applies it).
3. **Else:** run the existing apply logic, but treat it as reconciling toward
   `target` for `gen`: after the pre-flight `power_on` (and before issuing the
   screen command), if `screen_intent_gen != gen`, a newer request superseded this
   one → abort (return `Ok`; the newer one owns the outcome). Clear
   `screen_intent` only when finishing as the current generation.

**Reconcile hook** — at the two reconnect/`power_on` success points (after
`reconnecting = false` + `ConnectionChanged(true)`): if `screen_intent` is
`Some(target)`, spawn an apply of `screen_on`/`screen_off` for `target` (off the
reconnect path so it can't stall it). It runs with a fresh generation and, since
`reconnecting` is now false, actually drives the screen.

**Why this is correct for the scenarios:**
- *Cold-boot tangle:* greeter `screen_off` arrives while `on_startup`'s power-on
  has `reconnecting = true` → deferred (no duplicate pre-flight power-on). The
  latest intent is applied once at reconnect-complete.
- *Login during cold boot (the user's concern):* the keypress to authenticate is
  a resume edge → `NotifyGreeterIdle(false)` → `screen_on` → bumps the generation
  and sets intent `on`, deferred behind the same reconnect. At reconnect-complete
  the intent is `on`. The earlier `screen_off`, if it ever started, sees the bumped
  generation and aborts. No stale blank after login.
- *Coalescing:* `screen_intent` is overwritten (latest-wins); N deferred actions
  collapse to one apply. Cross-source supersession (greeter-off → session/resume-on)
  works because they share the sink — no login state needed in the daemon.

### Why not the simpler alternatives
- **pending `Option<bool>`, no generation:** fails the in-flight race — a
  `screen_off` whose *own* pre-flight `power_on` set `reconnecting` can run to
  completion *and* the superseding `screen_on` applies, order undefined. The
  generation check is what makes supersession deterministic.
- **defer = drop:** loses the away-user blank (single edge, never recovered).
- **defer = queue + replay:** the stale-after-login bug the user flagged.

## Scope / non-goals

- **In scope:** `screen_on` / `screen_off` coalescing + reconnect reconcile.
- **Out of scope:** `power_on` / `power_off` (they *drive* the reconnect; deferring
  them is nonsensical) — applied immediately as today.
- **Documented non-goal:** the away-user cold-boot case where `on_startup`'s
  `screen_on` is the genuine latest intent (no login to flip it) → the screen
  ends up on. Mitigated operationally by an `idle_timeout_seconds` longer than the
  cold-boot (the greeter idle then fires *after* `on_startup`, on an Active TV, no
  defer). Making the greeter idle a *latching* intent that re-asserts after
  `on_startup` is the larger per-DE-redesign follow-up
  (`docs/ideas/event-driven-power-management.md`), not this change.

## Files

- `crates/tisvlayd/src/tv_manager.rs`: `ManagedTv` fields; defer + generation in
  `screen_on`/`screen_off`; reconcile at the two reconnect-success points.
- Tests (`crates/tisvlayd/tests/`, mock backend): (a) `screen_off` issued while a
  TV is `reconnecting` is deferred, then applied on reconnect-complete; (b)
  off-then-on while reconnecting applies only `on` (coalesced, latest-wins);
  (c) a superseding `screen_on` makes an in-flight `screen_off` abort.

## Verification

- `just check`.
- Unit/harness tests above (drive `reconnecting` via the mock + the reconnect path).
- Hardware: cold reboot with a short timeout no longer leaves the screen tangled;
  logging in during a cold boot does not blank the panel afterward.

## Risks

- **Semantics of `screen_on`/`screen_off` when deferred:** they return `Ok` before
  the screen physically changes. Callers are already fire-and-forget (session/greeter
  events) or optimistic (GUI); confirm no caller depends on completion-on-return.
- **Re-entrancy:** the reconcile apply calls `screen_on`/`screen_off` again after
  `reconnecting` clears — must not re-defer (it won't: flag is false) or recurse
  into `power_on` unboundedly (TV is Active post-reconnect).
- **Generation check placement:** must cover every `await` boundary in the
  pre-flight + verify-retry loops, or a superseded action could still issue a stale
  command.

## Review outcome — design rejected, work deferred

Two independent reviews (impl-correctness + design/maintainability) found the
design above **does not work as written and is the wrong altitude for this PR**:

1. **Premise is false.** `on_startup`'s power-on calls `wake_and_connect_tv`
   *directly* (`tv_manager.rs:1238`) and never sets `reconnecting` — only
   `TvManager::power_on` does (`:2239`). So in the cold-boot scenario this plan
   targets, `is_reconnecting()` is `false`, the greeter `screen_off` is **not**
   deferred, and it runs its own pre-flight `power_on` — the exact duplicate-wake
   tangle the plan claimed to remove.
2. **Reconcile recurses into a WoL storm.** At the reconnect-success hook
   (`:2337`) the TV is merely connected, not Active (commonly `ActiveStandby`), so
   reconciling `screen_off` → pre-flight `power_on` → success → reconcile → … and
   re-sends WoL for the whole cold-boot window.
3. **Not spawnable as written.** `power_on`/`reconnect` take `&self`; there is no
   `Weak<Self>`, and adding `self: &Arc<Self>` collides with the `ActionExecutor`
   trait (`action.rs:21`). Plus TOCTOU between "set intent" and "test reconnecting"
   unless done in one locked critical section, and the generation check must sit at
   every `await` in the verify-retry loops, not once.
4. **Doesn't help the target user.** Even corrected, coalescing only fixes
   login-supersession. The short-timeout / away-user / cold-boot case (the one OLED
   protection is *for*) is unaffected, because `on_startup`'s `screen_on`
   (`:1262`) is the genuine latest intent. That case needs a *latching* greeter
   intent that re-asserts after `on_startup` — the larger per-DE redesign, not this.

**Revised decision:** this narrow coalescer is **superseded**, not merely deferred.
The review showed the real problem is that the daemon has no persistent notion of
*what the screen should be* — so a per-event coalescer can only ever patch one
slice. The work is reframed into a proper per-TV **desired screen state** model
spanning every boot/login/lock/idle context: see
[`screen-state-model.md`](screen-state-model.md), which maps the user stories and
fixes the goal/scope before any mechanism is designed.

This plan file is retained as the record of the rejected approach and the four
concrete failure modes above — useful input to the model design.
