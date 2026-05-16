# Make `screen_on` inherently reliable — drop the QoS abstraction

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
                ┌──────────────┐
session events ─►│ tv_manager   │
                │   .screen_on │  ── always: send screen_on,
system events ─►│ (always      │     verify cached PowerState,
                │  verifies +  │     retry, escalate to .power_on
                │  escalates)  │     if verify never sees "active"
                └──────────────┘
```

- `tv_manager::screen_on(tv_id)` runs verify-retry-escalate **unconditionally**. Inline pre-flight short-circuit on cached `"standby"`. No QoS branch, no separate helper.
- `tv_manager::power_on(tv_id)` unchanged from `main` (already has WoL+retry+reconnect).
- `tv_manager::screen_off(tv_id)` / `power_off(tv_id)` unchanged from `main` (fire-and-forget).
- **No `dispatch_action`**. Session and system event handlers call the right `tv_manager` method directly by matching on the resolved `SessionAction`.
- **No `Tv1.Dispatch`**. D-Bus surface stays `Tv1.Power.PowerOn` / `Tv1.Power.PowerOff` / `Tv1.Screen.SetScreen` — V2's diff vs `main` doesn't touch the D-Bus surface at all on the lifecycle side.

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

### Commit 3 — `Arc<dyn TvBackend>` registry refactor

Adapt from `a740ce7`. **Drop** the new `TvBackend::reliable_verify_delay()` / `reliable_max_attempts()` trait methods (those were QoS-tied). **Keep** the registry change to store `Arc<dyn TvBackend>` directly. Touches `tisvlay-backends/src/lib.rs` + the read sites in `tisvlayd`. Pure orthogonal cleanup.

### Commit 4 — `power_state` module + `last_known_power_state` field

Adapt from `3f1adad` (the post-Copilot dedup-refactor) + the parts of `9e31c63` / `97fa4bd` that introduced `ManagedTv::last_known_power_state`. New file `crates/tisvlayd/src/power_state.rs` exports `derive_power_state(&TvState) -> &'static str` + `ACTIVE` / `SCREEN_OFF` / `STANDBY` constants. Three call sites (`state_forwarder`, `run_subscription_task`, `register_tv_interfaces`'s initial-state seed) route through it. Subscription-task writer updates `last_known_power_state` on every push.

### Commit 5 — Mock infrastructure for verify-retry tests

Adapt from `11bbcf6` + the `set_screen_on_noop` knob from `6eceaea`. Touches `tisvlay-mock/src/{state,handle,connection}.rs`. Adds `inject_power_state`, `set_screen_on_noop`, plus the shared-state plumbing (`live_states`, `subscription_txs`).

### Commit 6 — Inherently-reliable `screen_on` + integration tests

The new content. Replaces `tv_manager::screen_on`'s current fire-and-forget body with the verify-retry-escalate loop inline. Includes:

- Pre-flight short-circuit on cached `STANDBY`.
- The retry loop with hardcoded `Duration::from_millis(300)` verify-delay + hardcoded `2` max attempts (no per-backend trait methods — we have one vendor).
- Escalation to `self.power_on(tv_id)` on continued failure.
- Tests in `tisvlayd/tests/dbus_contract.rs`: `screen_on_short_circuits_from_standby`, `screen_on_escalates_via_verify_retry_loop` (using `set_screen_on_noop`).
- Tests in `tisvlay-session/tests/session_events.rs`: end-to-end repro through the session event handler. `reliable_screen_on_escalates_when_tv_slipped_past_screen_off` (Standby injection → short-circuit), `..._slipped_to_active_standby` (ActiveStandby + `screen_on_noop` → escalate).
- Test in `tisvlayd/tests/profile_dispatch.rs`: `wake_with_screen_on_short_circuits` (system-event path).

### Commit 7 — Documentation

- New `docs/lg/firmware-quirks.md` (port from `feat/action-qos`, **strip the QoS-scope language**; the "10 s `REQUEST_MS` is load-bearing" section is the load-bearing part of this doc).
- Update `docs/design.md`: add a "Reliable `screen_on`" subsection under the per-TV interface section. No QoS namespace. No `Tv1.Dispatch`. The directory tree stays as `main` had it.
- Update `docs/testing.md`: "On-hardware triage tooling" subsection pointing at `scripts/triage_dispatch_perf.py`.
- Update `docs/ideas/tv-side-power-prevention.md`: re-port with corrected back-references (drop the `docs/plans/action-qos.md` link; cross-reference `docs/lg/firmware-quirks.md` and `docs/design.md` instead). Include the "Prior art" section.
- Port `scripts/triage_dispatch_perf.py` as-is.

### Commit 8 — Delete this plan doc

Standard convention from `docs/plans/README.md` — once shipped, plans get migrated and deleted. Most content from this doc lands in `docs/design.md` (the architecture) and `docs/lg/firmware-quirks.md` (the war stories); the rest is git-archived.

## What gets dropped from V1

- `Reliability` enum, `QosConfig`, `EventAction`, `ResolvedEventAction`, `ResolvedQos`, `default_reliability_for`.
- `tisvlay-core::TvBackend::reliable_verify_delay()` / `reliable_max_attempts()` trait methods.
- `Tv1.Dispatch(action, qos)` D-Bus method.
- `Tv1.GetEffectiveSystemProfile()` / `Session1.GetEffectiveUserProfile()` D-Bus methods (those returned the resolved QoS-shape JSON; without QoS, the resolved profile is just `Option<SessionAction>` per slot and the existing config-read paths suffice).
- `dispatch_action` / `dispatch_reliable_screen_on` helpers in `tv_manager`.
- Reliability-aware dedup carve-out in `tisvlay-session::event_handler`. Dedup keys on `(tv_id, action)` for `ScreenOff` / `PowerOff` (fire-and-forget — same-as-last-time skip is fine). `ScreenOn` and `PowerOn` always dispatch (verify-retry-escalate inside `tv_manager` handles "are we already there?"); dedup just isn't invoked for them.
- Profile TOML's `qos = { ... }` nested-table form. All `on_*` slots revert to bare `Option<SessionAction>`.
- Spike commits `f384f60` + `6845097` + `4159cba` + `083fb10` — drop entirely. They live on `feat/action-qos` for the design-exploration record; V2 starts from `main`.
- Plan/concerns/idea adjustments tied to QoS (e.g. the resolved QoS-shape JSON in `Session1` won't exist).

## Open questions

1. **Should `ScreenOff` also have inherent verify?** Today it's fire-and-forget. The symmetric argument: "a bright TV when you wanted dark isn't catastrophic, but it's still wrong" — verify could catch firmware glitches where `screen_off` returns Ok but the panel stays lit. The cost is one extra 300 ms wait + read per dispatch. Recommend: leave fire-and-forget for V2; revisit if field reports surface the inverse failure mode. (Note: the V1 asymmetric-default analysis already addressed this; we're inheriting that conclusion without the abstraction.)

2. **Should the verify-delay (300 ms) and max-attempts (2) be configurable?** V1 had them as per-backend trait methods. With one vendor, hardcoded daemon constants are fine. If Samsung lands with materially different SSAP-push timing, promote to constants in `power_state.rs` or a sibling daemon module — easy refactor.

3. **What about `Session1.GetEffectiveUserProfile` and `Tv1.GetEffectiveSystemProfile`?** V1 added these for GUI Profiles-pane introspection. Without QoS the resolved-profile JSON is just `{ "on_session_unlock": "screen_on", ... }` — much simpler. Open whether the GUI still needs the daemon to resolve this or can compute it client-side from `ListUserProfiles` + the TV's assigned profile. Probably keep `GetEffectiveUserProfile` (it's a fair API: "what action does this TV actually take on event X") but the implementation simplifies.

4. **`Tv1.Power.PowerCycleProgress` signal**: V1 added this for the GUI to surface "WoL → Connecting → Connected" phases during a manual power-on. It's orthogonal to QoS and survives V2; carry it over.

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
