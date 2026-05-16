# Make screen-state actions inherently reliable — drop the QoS abstraction

## Motivation

[`feat/action-qos`](https://github.com/ruffsl/TisVlay/pull/56) shipped working behavior for the user-reported "TV doesn't wake after lockscreen" bug, but on close inspection the design surface is mostly premature abstraction. The PR introduced a per-event `qos.reliability` namespace, a `Tv1.Dispatch(action, qos)` consolidated D-Bus method, asymmetric type-driven defaults, per-backend tunables on `TvBackend`, and a substantial resolve chain — all to encode **one bit on one action**: "is this `ScreenOn` verify-retry-escalate or fire-and-forget?"

Everywhere else, the `dispatch_action` match collapses with `(_, _)` wildcards. `PowerOn` is already inherently reliable (its `tv_manager::power_on` has WoL + connect-retry + reconnect inside). Sleep-side actions (`ScreenOff`, `PowerOff`) are fire-and-forget by design's intent. The "future dimensions (`deadline`, `durability`)" justification is YAGNI — we have zero concrete need, and a future "verify `power_on` reached `active`" tier would likely want a different shape anyway (verify-target, not retry-then-escalate).

This plan ships the same observable behavior — including the post-merge perf-triage lessons — with the QoS surface gone. `screen_on` becomes inherently reliable, mirroring how `power_on` already is.

## What's V1's spike `feat/action-qos` taught us

These survive the rewrite unchanged. The plan doc here is mostly about *what NOT to carry over*.

### Real bugs (orthogonal to QoS)

- **Bug A**: `PowerState::ActiveStandby` was mis-mapped to `is_screen_on = true` in `tisvlay_core::TvState::update_power`. ActiveStandby is the state LG OLED firmware enters when the user presses Power from Screen Off; mapping it to `screen_on=true` defeated any verify check. Fix: add it to the screen-off suppression list.
- **Bug B**: LG client's `screen_on` / `screen_off` masked every `TvError` as idempotent-success, swallowing the `-102 not in screen-off substate` signal the reliable tier needed. Fix: `is_idempotent_screen_error` discriminates by leading error code; only `500` is masked.

Both have regression tests. Both ship in V2.

### Load-bearing operational lesson

The LG client's `REQUEST_MS = 10_000` ms is **load-bearing** when the WS goes half-dead. During the wait, `with_retry` runs a reconnect attempt that physically "knocks" the TV awake, which lets the subsequent `power_on`'s WoL → Registration complete in <1 s. Three spike branches that tried to bypass this (timeout race, WS liveness probe, lowered `REQUEST_MS`) all measurably regressed. Documented exhaustively in [`docs/lg/firmware-quirks.md`](../lg/firmware-quirks.md). V2 inherits this doc and the autonomous reproducer ([`scripts/triage_dispatch_perf.py`](../../scripts/triage_dispatch_perf.py)).

### Pre-flight short-circuit

If cached `last_known_power_state == "standby"` (i.e., `is_on == false` — TV already past Screen Off), `screen_on` over the existing WS would no-op or stall; the helper short-circuits to `power_on` directly. This was V1's `dispatch_reliable_screen_on` short-circuit; in V2 it's just a branch inside `tv_manager::screen_on` because there's no separate "reliable" tier.

### Infrastructure

- `power_state` module: single source of truth for the `(is_on, is_screen_on) → "standby"/"screen_off"/"active"` derivation, with named constants used by the short-circuit comparison.
- `MockHandle::inject_power_state(address, PowerState)` — drives subscription pushes for tests.
- `MockHandle::set_screen_on_noop(address, enabled)` — models LG's "screen_on returns Ok but doesn't wake the panel" behavior so verify-retry-escalate tests don't silently pass via mock's default `is_screen_on=true` flip.
- `FakeSsapServer::set_command_error(uri, error_str)` — drives Bug B regression tests.
- `ManagedTv::last_known_power_state` — daemon-side cached state read by `screen_on`'s verify check (no D-Bus round-trip).
- `tisvlay-backends` registry storing `Arc<dyn TvBackend>` directly (replaces denormalized fields) — orthogonal cleanup, keep.

## V2 architecture

```
                ┌──────────────────┐
session events ─►│ tv_manager       │
                │  .screen_on(tv)  │  verify cached "active";
                │  .screen_off(tv) │  verify cached "screen_off";
system events ─►│  .power_on(tv)   │  WoL+retry+reconnect;
                │  .power_off(tv)  │  fire-and-forget.
                │                  │
                │  retry budget    │  per-backend RetryConfig
                │  + escalate via  │  (verify_delay, max_attempts)
                │  power_on if     │
                │  pre-flight sees │
                │  cached "standby"│
                └──────────────────┘
```

- `tv_manager::screen_on(tv_id)` runs verify-retry-escalate **unconditionally**. Inline pre-flight short-circuit on cached `"standby"` → straight to `power_on`. Target cached state: `"active"`.
- `tv_manager::screen_off(tv_id)` runs verify-retry-escalate **unconditionally**. Inline pre-flight on cached `"standby"`: TV must be powered on to be in screen-off substate, so escalate via `power_on` first, then send `screen_off`. Target cached state: `"screen_off"`. (Side effect: locking the desktop while the TV is already off briefly powers it on before blanking — rare and acceptable, since `on_startup = "screen_off"` is the load-bearing usecase this serves.)
- `tv_manager::power_on(tv_id)` unchanged from `main` (already has WoL + retry + reconnect inside).
- `tv_manager::power_off(tv_id)` unchanged from `main` (fire-and-forget; no meaningful escalation path).
- Verify-delay and max-attempts come from `TvBackend::verify_retry_config() -> RetryConfig` — single trait method returning a struct, defaulted to `{ verify_delay: 300ms, max_attempts: 2 }` (LG-tuned). Per-backend override point for transports with different latency characteristics (USB-CEC, RS-232, future Samsung).
- **No `dispatch_action`**. Session and system event handlers call the right `tv_manager` method directly by matching on the resolved `SessionAction`.
- **No `Tv1.Dispatch`**. D-Bus surface stays `Tv1.Power.PowerOn` / `Tv1.Power.PowerOff` / `Tv1.Screen.SetScreen` — V2's diff vs `main` doesn't touch the D-Bus surface at all on the lifecycle side.
- **No session-side `last_action` dedup.** Every action self-verifies via its pre-flight check, so a duplicate dispatch is at worst a fast no-op when cached state already matches the target. Removing the dedup simplifies `event_handler`.

### Profile config

```toml
[user.profiles.default]
on_session_lock   = "screen_off"   # fire-and-forget
on_session_unlock = "screen_on"    # always verify-retry-escalate
on_greeter_wake   = "screen_on"
on_idle_timeout   = "screen_off"
on_idle_resume    = "screen_on"
```

Bare-string action form (no `qos` table). `SystemProfileConfig` and `UserProfileConfig` revert to `Option<SessionAction>` (away from `Option<EventAction>`).

### Direct callers (tray, GUI, CLI)

`DaemonClient::screen_on(tv_id)` calls `Tv1.Screen.SetScreen(true)` — same as `main`. Users clicking a tray "wake TV" button get the verify-retry-escalate path implicitly, which is what they wanted anyway (a button labeled "wake TV" should wake the TV reliably). The ~300 ms verify-delay on first attempt is below human snap-back threshold; on a responsive TV, verify passes on attempt 1 and total latency is ~50 ms + 300 ms = ~350 ms — still feels instant.

## Implementation outline

Branch off `origin/main`. The plan is to land V2 in roughly the commit shape below; squash on merge.

### Commit 1 — Bug A: `ActiveStandby` mapping

Cherry-pick or re-apply [`b69edd2`](https://github.com/ruffsl/TisVlay/commit/b69edd2). Touches `tisvlay-core/src/state.rs` only. Adds `active_standby_marks_screen_off` + the table-driven `update_power_screen_on_mapping_is_complete` regression test.

### Commit 2 — Bug B: LG client error discrimination

Cherry-pick or re-apply [`8dc3f44`](https://github.com/ruffsl/TisVlay/commit/8dc3f44). Touches `tisvlay-lg/src/client.rs` + `test_server.rs` (the `set_command_error` knob). Adds `screen_on_propagates_minus_102_not_in_screen_off_substate` + `screen_on_masks_500_idempotent_error`.

### Commit 3 — `Arc<dyn TvBackend>` registry refactor + `RetryConfig` trait method

Adapt from `a740ce7`. Two changes:

- **Registry change**: store `Arc<dyn TvBackend>` directly (replaces the denormalized fields). Touches `tisvlay-backends/src/lib.rs` + the read sites in `tisvlayd`.
- **New `RetryConfig` struct in `tisvlay-core`** with `verify_delay: Duration` and `max_attempts: usize` fields, plus a single `TvBackend::verify_retry_config() -> RetryConfig` trait method defaulted to `{ 300ms, 2 }` (LG-tuned). Per-backend override point. Renamed away from V1's `reliable_*` prefix since there's no longer a `best_effort` sibling tier.

Unit tests pin the default values and exercise an overridden backend (matches V1's pattern).

### Commit 4 — `power_state` module + `last_known_power_state` field

Adapt from `3f1adad` (the post-Copilot dedup-refactor) + the parts of `9e31c63` / `97fa4bd` that introduced `ManagedTv::last_known_power_state`. New file `crates/tisvlayd/src/power_state.rs` exports `derive_power_state(&TvState) -> &'static str` + `ACTIVE` / `SCREEN_OFF` / `STANDBY` constants. Three call sites (`state_forwarder`, `run_subscription_task`, `register_tv_interfaces`'s initial-state seed) route through it. Subscription-task writer updates `last_known_power_state` on every push.

### Commit 5 — Mock infrastructure for verify-retry tests

Adapt from `11bbcf6` + the `set_screen_on_noop` knob from `6eceaea`. Touches `tisvlay-mock/src/{state,handle,connection}.rs`. Adds `inject_power_state`, `set_screen_on_noop`, plus the shared-state plumbing (`live_states`, `subscription_txs`).

### Commit 6 — Inherently-reliable `screen_on` and `screen_off` + integration tests

The new content. Replaces both methods' fire-and-forget bodies with verify-retry-escalate loops inline.

**`tv_manager::screen_on`**:
- Pre-flight short-circuit on cached `STANDBY` → dispatch `power_on` directly (which restores to `"active"` per LG's WoL behavior).
- Verify-retry loop: send `screen_on`, sleep `RetryConfig.verify_delay`, read cached state, target `"active"`. Retry up to `max_attempts`.
- Escalation on retry exhaustion: `self.power_on(tv_id)`.

**`tv_manager::screen_off`**:
- Pre-flight on cached `STANDBY`: TV must be on to be in screen-off substate, so call `self.power_on(tv_id)` first to bring it up active, then continue into the dispatch loop below.
- Verify-retry loop: send `screen_off`, sleep `RetryConfig.verify_delay`, read cached state, target `"screen_off"`. Retry up to `max_attempts`. No escalation past retries — TV is already reachable; if it refuses to blank, log and give up.

**Session-side dedup removal**: `event_handler` no longer tracks `last_action` per TV. Every dispatch self-verifies via the pre-flight check, so duplicate events are at worst a cheap cache read.

Tests:
- `tisvlayd/tests/dbus_contract.rs`: `screen_on_short_circuits_from_standby`, `screen_on_escalates_via_verify_retry_loop` (using `set_screen_on_noop`), `screen_off_escalates_via_power_on_when_standby` (new — Standby injected, screen_off called, asserts the TV ends up `"screen_off"` via `power_on` + screen_off composite), `screen_off_verify_retry_when_tv_refuses` (using `set_screen_off_noop` — new mock knob, mirrors the screen_on one).
- `tisvlay-session/tests/session_events.rs`: end-to-end repro through the session event handler. The existing two tests (Standby short-circuit, ActiveStandby + `screen_on_noop` escalate) keep the names that better describe what they actually exercise.
- `tisvlayd/tests/profile_dispatch.rs`: `wake_with_screen_on_short_circuits` (system-event path) + `on_startup_screen_off_powers_on_first` (system-event path verifying the `on_startup = "screen_off"` use case end-to-end).

### Commit 7 — Documentation + orthogonal carry-overs

- New `docs/lg/firmware-quirks.md` (port from `feat/action-qos`, **strip the QoS-scope language**; the "10 s `REQUEST_MS` is load-bearing" section is the load-bearing part of this doc). Update test-name references to V2's names.
- Update `docs/design.md`: add a "Reliable screen-state actions" subsection under the per-TV interface section. No QoS namespace. No `Tv1.Dispatch`. The directory tree stays as `main` had it (modulo `PowerCycleProgress` being noted, see below).
- Update `docs/testing.md`: "On-hardware triage tooling" subsection pointing at `scripts/triage_dispatch_perf.py`.
- Update `docs/ideas/tv-side-power-prevention.md`: re-port with corrected back-references (drop the `docs/plans/action-qos.md` link; cross-reference `docs/lg/firmware-quirks.md` and `docs/design.md` instead). Include the "Prior art" section.
- Port `scripts/triage_dispatch_perf.py` as-is.
- Port `docs/plans/README.md` (the convention doc — plans live in `docs/plans/` while active, migrate-and-delete on merge). Has standing value regardless of which PR ships first.
- Carry over `Tv1.Power.PowerCycleProgress` signal — orthogonal to QoS, useful for GUI wake-progress UX. Listed in the design.md directory tree's Tv1 signal line.

### Commit 8 — Delete this plan doc

Standard convention from `docs/plans/README.md` — once shipped, plans get migrated and deleted. Most content from this doc lands in `docs/design.md` (the architecture) and `docs/lg/firmware-quirks.md` (the war stories); the rest is git-archived.

## What gets dropped from V1

- `Reliability` enum, `QosConfig`, `EventAction`, `ResolvedEventAction`, `ResolvedQos`, `default_reliability_for`.
- `Tv1.Dispatch(action, qos)` D-Bus method.
- `Tv1.GetEffectiveSystemProfile()` / `Session1.GetEffectiveUserProfile()` D-Bus methods. The GUI Profiles pane (M4) will compute its "explicit-set vs inherited-fallthrough" display client-side from `ListSystemProfiles` / `ListUserProfiles` + the known inheritance order. If client-side computation turns out cumbersome during M4 prototyping, design the right replacement D-Bus method then with the right shape informed by the actual GUI consumer.
- `dispatch_action` / `dispatch_reliable_screen_on` helpers in `tv_manager` (their bodies fold into `screen_on` / `screen_off` directly).
- Entire `last_action` dedup map in `tisvlay-session::event_handler`. Self-verifying actions make it redundant.
- Profile TOML's `qos = { ... }` nested-table form. All `on_*` slots revert to bare `Option<SessionAction>`.
- Spike commits `f384f60` + `6845097` + `4159cba` + `083fb10` — drop entirely. They live on `feat/action-qos` for the design-exploration record; V2 starts from `main`.
- The trait-method rename: V1's `reliable_verify_delay()` / `reliable_max_attempts()` become a single `verify_retry_config() -> RetryConfig`. Same defaults, simpler surface (one method, one struct, both magic values in one place).

## Resolved decisions

1. **`screen_off` is inherently reliable too.** Symmetric with `screen_on`. The load-bearing usecase: `on_startup = "screen_off"` — for the TV to be in screen-off substate, it must be powered on, so reliable `screen_off` escalates via `power_on` when cached state is `"standby"`. Side effect (acceptable): `on_session_lock = "screen_off"` with TV already off briefly powers it on before blanking. Rare scenario; if it surfaces as a complaint we'd revisit with explicit per-event semantic primitives rather than reintroducing QoS.

2. **Per-backend tunables via `RetryConfig` struct.** A single `TvBackend::verify_retry_config() -> RetryConfig { verify_delay, max_attempts }` trait method with defaults `{ 300ms, 2 }`. Per-backend so future transports (USB-CEC, RS-232) with different retry semantics can override; struct shape so adding a third tuning knob (e.g., escalation budget) doesn't churn the trait surface.

3. **Drop `GetEffectiveSystemProfile` / `GetEffectiveUserProfile` for V2.** The GUI Profiles pane's actual UX need is "what did this profile explicitly set per slot, and what would it fall through to if unset" — a richer surface than `GetEffective`'s resolved-only return. Computing it client-side from `ListSystemProfiles` / `ListUserProfiles` + the known inheritance order is straightforward when M4 lands. If client-side cumbersome in practice, design the right replacement method then with the right shape informed by the prototype.

4. **`Tv1.Power.PowerCycleProgress` signal survives.** Orthogonal to QoS; useful for the GUI's wake-progress UX. Documented in the design.md directory tree on the Tv1 signal line.

## Future work (out of scope for V2)

- **Pre-login greeter wake event.** Today's `on_greeter_wake` fires from `tisvlay-session`, which runs per-user — so it only catches greeter activity *during* a session lock (post-login). The morning-after-overnight-reboot UX (`on_startup = "screen_off"` to avoid burn-in, then unblank when the user touches the SDDM/display-manager greeter pre-login) needs new plumbing: either a `tisvlay-greeter-session` user service running as the greeter user, or system-bus detection of display-manager input events. File as a separate idea doc once V2 ships.
- **Per-event semantic primitives for `screen_off`.** If the "lock-while-TV-already-off briefly powers the TV on" side effect ever surfaces as a real user complaint, the right answer isn't to reintroduce QoS but to add explicit action types: `EnsureScreenOff` (composite: power_on if needed, then screen_off — what V2's `screen_off` does today) vs. `ScreenOffIfReachable` (fire-and-forget — V1's behavior). Defer until field reports justify the additional surface.
- **Replacement for `GetEffective*` if client-side computation proves cumbersome.** Design informed by M4 GUI prototyping.

## Acceptance criteria

- [ ] `just check` is green.
- [ ] Manual repro on LG OLED G4: lock → press Power on remote → mouse-move → TV wakes back up within ~8 s.
- [ ] D-Bus surface vs `main`: zero net change to `Tv1.Power` / `Tv1.Screen` method shapes. (V2's diff vs `main` is in the daemon implementation, not the wire format.)
- [ ] Profile TOML stays the simple bare-string form. No `qos` table. No nested `{ action, qos }` shape.
- [ ] `tv_manager::screen_on`'s body contains the verify-retry-escalate loop directly. No `dispatch_action`, no `dispatch_reliable_screen_on` helper, no QoS branch.
- [ ] Tests cover: short-circuit-from-standby, verify-retry-escalate (with `screen_on_noop`), system-event reliable wake, Bug A regression, Bug B regression.
- [ ] `docs/lg/firmware-quirks.md` is ported (with QoS-scope language stripped).
- [ ] `scripts/triage_dispatch_perf.py` is ported.

## Reference

- Original spike PR: [`feat/action-qos`](https://github.com/ruffsl/TisVlay/pull/56). Useful for cherry-pick references and the field-test trace data; do not merge.
- Field-test repro and the "10 s `REQUEST_MS` is load-bearing" finding: lives in `docs/lg/firmware-quirks.md` on `feat/action-qos`; port verbatim with the QoS-scope edits noted above.
