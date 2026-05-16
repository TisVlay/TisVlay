# Relocate QoS enforcement from session to daemon

## Motivation

The Action QoS PR ([docs/plans/action-qos.md](action-qos.md)) landed the reliable dispatch tier as a helper inside `tisvlay-session::lib.rs::reliable_screen_on`. PR review surfaced architectural leakage that echoes the patterns [PR #2](https://github.com/ruffsl/TisVlay/pull/2), [PR #8](https://github.com/ruffsl/TisVlay/pull/8), and [PR #25](https://github.com/ruffsl/TisVlay/pull/25) spent effort unwinding — retry / coordination logic living in a layer that has to do round-trips to fetch the data it needs.

### What's wrong with the current placement

1. **Three+ D-Bus round-trips per reliable dispatch.** `reliable_screen_on` calls `daemon.screen_on(tv_id)`, then `daemon.power_state(tv_id)` to verify, then retries (more round-trips), then falls through to `daemon.power_on(tv_id)`. Every leg crosses IPC; the daemon already has the cached `PowerState` property in-process.
2. **Vendor-tuned constants in the session crate.** `RELIABLE_VERIFY_DELAY = 300 ms` is tuned to LG's `getPowerState` subscription latency; the comment even names LG's `-102` error code. These belong with the backend (or at minimum near the dispatch path), not in the session-policy layer.
3. **System-event asymmetry.** `tv_manager::execute_action` (the daemon-side dispatch for `on_startup`, `on_wake`, etc.) has NO equivalent reliable tier. Today this works out by accident because `power_on` already has WoL+retry inside `tv_manager::power_on`, but the moment a user configures `on_wake = screen_on` or we add a system-side action that needs reliability, we'd have to either duplicate `reliable_screen_on` in `tv_manager` (the exact copy-paste pattern those linked PRs unwound) or send a D-Bus call from the daemon to itself.
4. **Future QoS dimensions multiply the cost.** `qos.deadline`, `qos.durability` (placeholders in the v1 plan) each have to be plumbed through D-Bus from session to daemon — yet the daemon is where the dispatch primitives and cached state live.

The reliable-tier state machine — verify cached state, retry, escalate `screen_on` → `power_on` — has every input on the daemon side. The session service today is the policy layer (which event → which action → which tier), but the *enforcement* of that policy belongs in the daemon.

## Target architecture

```
                    ┌─────────────┐
   Session events ─►│             │
                    │  daemon's   │       in-process:
                    │  dispatch   │◄────  cached PowerState
   System events ──►│  helper     │       backend connection
                    │  (1 impl)   │       vendor-tuned constants
                    └─────────────┘
                          ▲
                          │ D-Bus  (Tv1.Action.Dispatch)
                          │
                    ┌─────────────┐
                    │  session    │       policy:
                    │   lib       │       resolved EventAction
                    │  (caller)   │       (action + qos)
                    └─────────────┘
```

**Single enforcement site.** A `dispatch_reliable_action` helper in `tv_manager` (or a sibling module) implements verify-retry-escalate using direct in-process reads of cached state. Both the session-event D-Bus entry point AND the daemon-internal system-event dispatch consume it.

**Thin session lib.** `tisvlay-session::execute_action_for_tv` reads the resolved `EventAction` from the profile and calls a single new D-Bus method (`Tv1.Action.Dispatch(action, qos)`), passing the QoS tier through. No retry-loop logic in the session crate.

**Direct callers untouched.** Existing methods (`Tv1.Screen.SetScreen`, `Tv1.Power.PowerOn`, etc.) stay best-effort for the tray / GUI / CLI paths that don't want verify overhead on a manual click.

## Decisions

1. **D-Bus surface — drop the redundant per-feature methods.** No back-compat constraints; cleanest answer is one entry point.
   - DROP: `Tv1.Power.PowerOn`, `Tv1.Power.PowerOff`, `Tv1.Screen.SetScreen`.
   - ADD: `Tv1.Action.Dispatch(action: s, qos: s)` on a new `org.tisvlay.Tv1.Action` interface (or directly on `Tv1` — see open detail below).
   - KEEP: `Tv1.Power.PowerState` and `Tv1.Screen.IsScreenOn` read-only properties. Also every non-lifecycle-action method (`SetVolume`, `SetMuted`, `SetBacklight`, `SwitchInput`) — these aren't `SessionAction` values, have no QoS tier, and stay as direct commands.
2. **QoS encoding — JSON string.** Matches the `GetEffectiveSystemProfile` / `GetEffectiveUserProfile` precedent already on `Tv1` / `Session1`. New dimensions (`deadline`, `durability`) slot into the JSON without a wire-format change.
3. **Vendor-tuned constants — per-backend trait, with defaults.** Two new methods on the `TvBackend` metadata trait, both defaulted so existing backends don't have to override:
   ```rust
   pub trait TvBackend: Send + Sync + 'static {
       // ... existing methods ...

       /// How long to wait after a state-change command before verifying
       /// via cached PowerState. Tuned to the backend's push-update
       /// latency. LG's getPowerState subscription is sub-100 ms; the
       /// 300 ms default gives the cache a comfortable window.
       fn reliable_verify_delay(&self) -> std::time::Duration {
           std::time::Duration::from_millis(300)
       }

       /// Max `screen_on` attempts before the reliable path escalates
       /// to `power_on`. Default 2.
       fn reliable_max_attempts(&self) -> usize {
           2
       }
   }
   ```
   When Samsung / Sony / etc. land with diverging timings, they override the methods and nothing else changes. Choosing this over daemon-level constants sidesteps a future refactor — we design for the long term, not for v1's single-vendor reality.
4. **System events — direct in-process call to the helper.** No D-Bus self-call from `tv_manager`. The helper is parametrized over the dispatch primitives (a small set of closures or a trait), and `tv_manager::execute_*` plus the `Tv1.Action.Dispatch` D-Bus handler share the same code path.
5. **Rust `DaemonClient` — convenience wrappers stay, D-Bus wrappers consolidate.**
   - KEEP: `daemon.screen_on(tv_id)`, `daemon.screen_off(tv_id)`, `daemon.power_on(tv_id)`, `daemon.power_off(tv_id)` as Rust convenience wrappers in `tisvlay-core::dbus_client`. Internal implementation flips to `dispatch_action(tv_id, "screen_on", BEST_EFFORT_QOS)` etc. — single D-Bus method on the wire, ergonomic Rust call sites unchanged.
   - ADD: `daemon.dispatch_action(tv_id, action, qos_json)` for explicit-QoS callers (session lib).
   - Effect: tray, GUI, CLI, integration tests using `daemon.screen_on(...)` keep compiling and behaving as before. Only the session lib's call sites change shape, since it's the one site that wants explicit QoS control.

### Open detail: `Tv1.Action.Dispatch` vs `Tv1.Action.Dispatch` interface placement

Two equivalent shapes:
- New interface `org.tisvlay.Tv1.Action` exposing `Dispatch(action: s, qos: s)` — symmetric with `Tv1.Power`, `Tv1.Screen` (each a separate interface).
- Method directly on `org.tisvlay.Tv1` (the base interface) alongside `Reconnect`, `RePair`, `Remove`, `GetConfig`, `UpdateConfig`, `GetEffectiveSystemProfile` — these are all lifecycle / control methods.

The base interface fits better — `Dispatch` is a lifecycle action like `Reconnect` and `RePair`, not a feature-bound command like `Volume` or `Input`. Recommend putting it directly on `Tv1`. Confirm during implementation.

## Implementation outline

Six commits, sequenced so each one is reviewable and the orphan-plumbing rule holds (every scaffolding commit gets its first user in the same commit):

1. **`TvBackend` trait gains `reliable_verify_delay` / `reliable_max_attempts` (with defaults).** LG and mock keep the defaults (the 300 ms / 2 attempts that the v1 plan picked). Add unit tests in `tisvlay-core` that exercise the default values and confirm a backend can override them. This is the only scaffolding commit; its first user lands in commit 2.

2. **Daemon-side helper + new `Tv1.Dispatch` D-Bus method + DaemonClient wrappers re-routed + integration test.** Add `tv_manager::dispatch_reliable_action(tv_id, action, qos)` (verify-retry-escalate, in-process cached-state reads, pulls constants from `backend.reliable_verify_delay()` / `backend.reliable_max_attempts()`). Add `Dispatch(action: s, qos: s)` method to the base `Tv1` interface (alongside `Reconnect`, `RePair`, etc.). Add `DaemonClient::dispatch_action(tv_id, action, qos_json)` proxy method. **Re-implement** `DaemonClient::{screen_on, screen_off, power_on, power_off}` as one-liner Rust wrappers that call `dispatch_action(...)` with `{"reliability":"best_effort"}`. **Remove** the now-dead D-Bus methods `Tv1.Power.PowerOn`, `Tv1.Power.PowerOff`, `Tv1.Screen.SetScreen` from both daemon and proxy. Integration test in `tisvlayd/tests/dbus_contract.rs`: drive the new `Dispatch` method with reliable qos + `MockHandle::inject_power_state(ActiveStandby)` to verify the escalation fires.

3. **Migrate session-event dispatch to call `dispatch_action` directly.** `tisvlay-session::execute_action_for_tv` becomes a one-liner: `daemon.dispatch_action(tv_id, action_str, qos_json).await`. Remove `reliable_screen_on`, `POWER_STATE_ACTIVE`, `RELIABLE_VERIFY_DELAY`, `RELIABLE_SCREEN_ON_ATTEMPTS` from `tisvlay-session/src/lib.rs`. The two existing session_events integration tests still pass since the end-to-end path is equivalent.

4. **Wire system-event dispatch (daemon-internal) to the same helper.** `tv_manager::execute_*` (or wherever the daemon's system-event dispatch lives) calls `dispatch_reliable_action` directly (in-process, no D-Bus). System events like `on_wake = screen_on` now get the reliable tier where today they'd silently fall through to best-effort. Add a `tisvlayd/tests/profile_dispatch.rs` case: set a system profile with `on_wake = { action = "screen_on" }`, fire the wake event, inject `ActiveStandby` before the verify window, assert escalation.

5. **Tests + cleanup.** Add a daemon-direct test for the helper that exercises it without going through D-Bus (calls `tv_manager::dispatch_reliable_action` with a mock backend). Sweep the workspace for any tray / GUI / CLI call sites that touched `Tv1.Power.PowerOn` / `Tv1.Screen.SetScreen` directly (rather than via `DaemonClient`) — they should be zero, but confirm.

6. **Docs.** Update [docs/plans/action-qos.md](action-qos.md) field-test verification subsection: swap "session-side helper" → "daemon-side helper", note that the same helper handles system events. Update [docs/design.md](../design.md) Action QoS section + D-Bus surface listings (Dispatch added to `Tv1`; PowerOn / PowerOff / SetScreen removed from `Tv1.Power` / `Tv1.Screen`). Update `docs/testing.md` if any wire-format examples reference the old methods.

## Out of scope for this refactor

- The vendor-trait stub for TV-side prevention (still deferred per [docs/ideas/tv-side-power-prevention.md](../ideas/tv-side-power-prevention.md)).
- New QoS dimensions (`qos.deadline`, `qos.durability`). The JSON-string wire format makes adding them trivial later.
- GUI Profiles pane wiring (M4).
- Per-backend tuning beyond the verify delay and attempt count. Future dimensions of "how does this backend reliably wake" (e.g., wake retry budget, WoL packet count) get separate trait methods as the need arises.

## Acceptance

- [x] All existing tests pass after the migration (365 today; net delta should be 0 or slightly positive from the new test cases).
- [x] The field-test repro (lock → remote power-off → mouse → TV wakes) still works end-to-end on the LG OLED G4.
- [x] No retry-loop / verify logic in the `tisvlay-session` crate; that crate's role is reduced to "read profile, call `dispatch_action`".
- [x] System-event dispatch (`on_wake`, `on_startup`, etc.) goes through the same helper. Configuring `on_wake = { action = "screen_on" }` would get reliable behavior identical to a user-session wake.
- [x] D-Bus surface shrinks: `Tv1.Power.PowerOn`, `Tv1.Power.PowerOff`, `Tv1.Screen.SetScreen` removed; `Tv1.Dispatch` added. `busctl --user tree org.tisvlay.Daemon` reflects the new shape.
- [x] `tisvlay-session/src/lib.rs` net LOC drops; `crates/tisvlayd/src/tv_manager.rs` (or sibling) net LOC grows by roughly the moved+adapted amount.
- [x] `tisvlay-core` `TvBackend` trait gains `reliable_verify_delay` / `reliable_max_attempts` with defaults; LG and mock leave them at default; tests exercise the override path.

## Post-merge perf triage

Pre-merge manual smoke testing on the LG OLED G4 surfaced a reported wall-clock regression: lock → remote power-off → mouse-move took ~14 s on the refactored daemon vs ~1 s on the pre-refactor baseline, reproduced across multiple trials by hand. The two paths share the same `tv_manager::screen_on → with_connection → backend.screen_on` code, so an A/B regression in that path would have surprised. Triage steps and outcome captured here so the next person doesn't have to rediscover the conclusion.

### Tools built

1. **Autonomous reproducer** — [scripts/triage_dispatch_perf.py](../../scripts/triage_dispatch_perf.py). Reads the daemon's TOML config, opens an out-of-band SSAP WebSocket using the daemon's existing `client_key`, sends `ssap://system/turnOff` to drop the TV into `screen_off`, then issues `Tv1.Dispatch` (or pre-refactor's `Tv1.Screen.SetScreen` via auto-detection) and times `t0 → PowerState == "active"` via `busctl monitor`. Cleanup forces a `Tv1.Reconnect` so any client-key permission churn from the script's own register is restored to the daemon's full-manifest set. Useful for repeat measurement of cold-start screen_on latency from a known TV state without driving the actual desktop lock/unlock flow.

2. **Inline tracing instrumentation** (held on a local branch, not landed). Elevated `tisvlay-lg::connection::{send, reader_loop}`'s per-frame `trace!` to `info!` with `target: "lgws"` and `(id, msg_type, uri)` fields, and added matching `target: "tvmtx"` traces around `tv_manager::with_connection`'s per-TV mutex acquire/release and `run_subscription_task`'s lock + write of `last_known_power_state`. Together these capture every WS tx/rx with monotonic timestamps and every per-TV mutex acquire latency — enough to see exactly which side stalls when a smoke-test trial does go slow.

### Findings

- **The autonomous probe did sometimes hit a 10 s `REQUEST_MS` path**, but it did so on **both** branches with similar frequency — the SSAP-turnoff inducer triggers stale-WS scenarios that aren't representative of the manual smoke-test flow. The autonomous data alone is therefore inconclusive about refactor cost.

- **Instrumented manual smoke tests on B and A came in essentially equivalent.** B: ~9.8 s end-to-end (GreeterWake → TV online). A: ~9.0 s and ~8.7 s across two trials. Both paths got `MSG_TYPE=Error` back from `turnOnScreen` in ~10 ms (LG client masks via idempotent-500 logic), the reliable verify-retry loop ran in ~600 ms exactly as designed, and the WoL → registration → online tail dominated the wall-clock. No 10 s `REQUEST_MS` hit in any of the instrumented A trials.

- **The originally-reported "consistent" regression did not reproduce under instrumented sampling.** Either the slow-A pattern is intermittent and we need more trials to catch one, or it was a small-N coincidence in the pre-instrumentation manual runs. Either way, the refactor isn't deterministically responsible for the variance.

### Decision

Independent of root cause: ship a **pre-flight short-circuit** in `dispatch_reliable_screen_on`. If `last_known_power_state` is already past Screen Off (i.e., `standby` — the daemon's derived shorthand for the TV being more deeply asleep than just panel-blank), skip the `screen_on` attempt and dispatch `power_on` directly. Costs nothing on the happy path (cached state is `screen_off` for a normal lockscreen wake), but **erases the 10 s `REQUEST_MS` exposure** any time the TV has slipped to deep standby while we weren't looking — which is the user-reported failure mode regardless of which branch we're on.

This makes the variance question moot: even if a future change re-introduces the slow path on either branch, this short-circuit makes it not matter. The inline instrumentation was reverted before merge; re-apply from the change set captured in this PR's discussion if the pattern resurfaces and you want fresh data.

### Follow-up: two cold-WS fix attempts, both reverted

Post-short-circuit smoke testing on the LG OLED G4 produced a slow trial that the short-circuit *didn't* catch — cached `PowerState` stayed at `"screen_off"` (so the deeper-standby predicate didn't fire), but the WS itself had gone half-dead with the TV. `screen_on` over that socket stalled the full 10 s `REQUEST_MS` before erroring out, recovery total ~13 s. We sketched three candidate fixes and ran the first two as spike branches against the live hardware. Both are archived on this branch as cherry-pick + revert pairs so future readers can read the diff. The third was deferred without implementation; see below.

#### Spike A — `screen_on` timeout race ([archived](../../crates/tisvlayd/src/tv_manager.rs) at commits `f384f60` + `6845097`)

Wrap `self.screen_on(tv_id)` inside the verify-retry loop with `tokio::time::timeout(Duration::from_secs(2), ...)`. Happy path returns in ~10 ms; 2 s is well clear of that and well under `REQUEST_MS`. On timeout, treat as a backend `Err` — break the loop and escalate to `power_on`.

**Result: regression.** Across four smoke-test trials:

| Trial | Path                              | Δ vs pre-fix 13 s slow |
|-------|-----------------------------------|------------------------|
| 1     | Fast `500` (TV responsive)        | 9 s                    |
| 2     | Timed out → power_on              | **15 s (+2 s)**        |
| 3     | Timed out → power_on              | **17 s (+4 s)**        |
| 4     | Timed out → power_on              | **15 s (+2 s)**        |

The 2 s timeout *did* fire as designed. But the recovery that follows it is consistently slower than the pre-fix recovery. Breakdown of trial 2:

```
22:04:45  Dispatch
22:04:47  screen_on timed out (+2 s)        ← fix fired
22:04:49  Direct-connect probe failed (+4 s)
22:04:49  WoL packet sent
22:04:54  Registration response (+9 s)      ← 5 s after WoL
22:05:00  Subscribed to backend updates (+15 s)
```

Compare to the pre-fix slow path where `WoL → Registration → TV online` happened in 1 s. The 10 s `REQUEST_MS` turned out to have an accidental side effect: while it stalled, the LG client's `with_retry` ran a reconnect attempt against the existing WS, which physically "knocked" the TV awake. By the time `power_on` ran, the TV was already responsive enough that registration was nearly instant. Cancelling `screen_on` early via `tokio::time::timeout` skipped that knock — the TV slipped deeper, and full WoL recovery took 9 s instead of <1 s.

Net effect: 8 s saved on `screen_on`, ~10 s lost on recovery. Slight pessimization.

#### Spike B — WS liveness predicate ([archived](../../crates/tisvlayd/src/tv_manager.rs) at commits `4159cba` + `083fb10`)

Track `last_rx_at` in `tisvlay-lg::connection::Connection` (`Arc<parking_lot::Mutex<Option<Instant>>>` updated by `reader_loop` on every Text/Ping/Close frame). Expose via a new `TvConnection::last_activity_at()` trait method (default `None`). Add a second short-circuit predicate to `dispatch_reliable_screen_on`: if `last_activity_at` is older than 5 s, skip `screen_on` and dispatch `power_on` directly. Reasoning: detect a half-dead socket *before* it stalls, then escalate cleanly with the WS torn down in `power_on`'s prologue.

**Result: same shape of regression, plus a new pathological case.** Five smoke-test trials:

| Trial | Path                              | Total |
|-------|-----------------------------------|-------|
| 1     | Stale-WS short-circuit            | **18 s** |
| 2     | Stale-WS short-circuit            | **12 s** |
| 3     | Stale-WS short-circuit            | **14 s** |
| 4     | Stale-WS short-circuit            | **14 s** |
| 5     | Threshold miss → `REQUEST_MS`     | **28 s** |

When the predicate fires (4/5), wall-clock is comparable to the pre-fix 13 s — the predicate works, but again at a net cost. Same root cause as Spike A: skipping `screen_on` loses the `with_retry` knock, so `WoL → Registration` takes 9 s instead of <1 s. The total is dominated by recovery time, not the latency saved on the bypassed `screen_on`.

When the predicate *misses* (trial 5, where the WS went quiet exactly 5 s before the wake — at the threshold boundary), the slow path takes **28 s**. This is the worst result of any branch we tried: `REQUEST_MS` fires AND the subsequent recovery is the slower flavor we see on the short-circuit path. Lowering the threshold to 3 s would catch this case but presumably false-positive on healthy short pauses, adding latency to the happy path.

#### Spike C — `SO_KEEPALIVE` (deferred, not implemented)

Aggressive TCP keepalive timing (`TCP_KEEPIDLE=5`, `TCP_KEEPINTVL=2`, `TCP_KEEPCNT=2`) would let the kernel detect half-dead sockets in ~9 s instead of waiting for the LG client's `REQUEST_MS`. The kernel-level detection would surface as `EPIPE` to the WS reader, which mirrors the natural `REQUEST_MS` path (`with_retry` reconnects, "knocks" the TV).

**Predicted result:** Spike C *might* preserve the knock effect since detection still surfaces through the same `with_retry` reconnect path the original `REQUEST_MS` triggers. The Spike A/B failure mode (skipping `screen_on` entirely → losing the knock) doesn't apply here. But the blast radius is wider — every backend WS on the daemon would inherit aggressive keepalive timing, not just reliable-tier dispatch — and the implementation has to extract the raw fd through `MaybeTlsStream`'s TLS wrapping, which is fiddly. Given how much complexity Spikes A and B added for no net win, the value-to-risk ratio doesn't justify proceeding without a clearer signal that the 13 s floor is itself unacceptable.

#### Decision

Accept the ~13 s pre-fix slow path as the floor. Keep the deeper-standby short-circuit (`97fa4bd`) — it correctly handles the "TV auto-powered-off to deep standby" case where `screen_on` would unambiguously fail, and the knock mechanism wouldn't help anyway in that state. Don't ship Spike A or B; their archived cherry-pick + revert pairs are the receipt.

The underlying lesson: **the 10 s `REQUEST_MS` isn't a bug, it's load-bearing.** It's how the LG client + TV firmware negotiate a wake-from-half-dead-WS state — the natural-feeling slow path is shorter end-to-end than any of the surgical workarounds because it bundles WS-recovery and TV-warm-up into a single 10 s wait. Any future fix that *bypasses* `screen_on` in a half-dead-WS scenario should be benchmarked against Spike A's and B's results before shipping; if it doesn't beat 13 s, it's a pessimization no matter how clean it reads.
