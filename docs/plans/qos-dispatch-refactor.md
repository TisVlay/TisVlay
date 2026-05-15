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

## Design questions (review before implementing)

1. **D-Bus surface — `Tv1.Action.Dispatch` vs extending existing methods.**
   - **Option α:** New method `Tv1.Action.Dispatch(action: s, qos: a{sv})` on a new interface. One method to plumb; future-proof for new dimensions; clean separation from manual-dispatch path.
   - **Option β:** Add a `qos: a{sv}` parameter to existing `SetScreen`, `PowerOn`, etc. Wider blast radius (all methods change shape); same wire-format every-method-takes-qos awkwardness.
   - **Recommendation:** Option α.

2. **QoS encoding on the wire — typed dict vs JSON string.**
   - `a{sv}` (D-Bus dict of `String → Variant`): native, but variant typing across dimensions (string vs duration vs bool) is awkward.
   - `String` carrying JSON: future dimensions slot in trivially; loses static typing at the wire.
   - **Recommendation:** JSON string. Matches the `GetEffectiveSystemProfile` / `GetEffectiveUserProfile` precedent in this PR.

3. **Vendor-tuned constants — daemon-level vs per-backend.**
   - Hardcoded in daemon for v1, matching today's session-crate placement (just moved).
   - Per-backend via a `TvConnection::reliable_verify_delay() -> Duration { Duration::from_millis(300) }` trait method.
   - **Recommendation:** Daemon-level for the refactor. Promote to per-backend when a second non-LG backend lands and the constants diverge.

4. **Direct dispatch from `tv_manager` (system events) — uses helper directly or via D-Bus self-call.**
   - In-process call to the helper. No D-Bus round-trip; helper takes references it can use.
   - **Recommendation:** Direct call.

5. **What does the session lib's `DaemonClient` look like after the refactor.**
   - New `DaemonClient::dispatch_action(tv_id, action, qos)` method invoking `Tv1.Action.Dispatch`.
   - The existing `daemon.screen_on / screen_off / power_on / power_off` wrappers can stay for direct callers but session lib stops using them.

## Implementation outline

Six commits, sequenced so each one is reviewable and the orphan-plumbing rule holds (every scaffolding commit gets its first user in the same commit):

1. **Daemon-side helper + D-Bus method + DaemonClient wrapper + integration test.** Add `tv_manager::dispatch_reliable_action` (verify-retry-escalate, in-process state reads, vendor-tuned constants). Add `Tv1.Action.Dispatch(action: s, qos: s)` D-Bus method that invokes it. Add `DaemonClient::dispatch_action(tv_id, action, qos)` proxy wrapper. Test: drive the new method from `tisvlayd/tests/dbus_contract.rs` via `inject_power_state` to verify the escalation fires.

2. **Migrate session-event dispatch to call the new method.** `tisvlay-session::execute_action_for_tv` becomes a one-liner that calls `daemon.dispatch_action(tv_id, action, qos_json)`. Remove `reliable_screen_on`, `POWER_STATE_ACTIVE`, `RELIABLE_VERIFY_DELAY`, `RELIABLE_SCREEN_ON_ATTEMPTS` from the session crate. The two existing session_events integration tests still pass since the end-to-end path is equivalent.

3. **Wire system-event dispatch to the helper.** `tv_manager::execute_*` (or the equivalent system-event dispatch chain) calls `dispatch_reliable_action` directly (in-process, no D-Bus). System events like `on_wake = screen_on` now get the reliable tier where today they'd silently fall through to best-effort.

4. **Update `Session1.GetEffectiveUserProfile` consumers and any GUI code** that needs to see the new wire format (probably no-op since the JSON shape is unchanged; just sanity-check).

5. **Tests + cleanup.** Add a daemon-direct test for the helper (no D-Bus, just call `tv_manager::dispatch_reliable_action` with a mock backend). Drop redundant assertions in the session integration tests that were exercising the now-removed retry loop directly.

6. **Docs.** Update [docs/plans/action-qos.md](action-qos.md) field-test verification subsection: replace "session-side helper" prose with "daemon-side helper". Update [docs/design.md](../design.md) Action QoS section similarly. Note the new `Tv1.Action.Dispatch` method in the D-Bus surface listings.

## Out of scope for this refactor

- The vendor-trait stub for TV-side prevention (still deferred per [docs/ideas/tv-side-power-prevention.md](../ideas/tv-side-power-prevention.md)).
- New QoS dimensions (`qos.deadline`, `qos.durability`). The JSON-string wire format makes adding them trivial later.
- Per-backend tuning of the verify delay. Promote when a second backend's constants diverge from LG's.
- GUI Profiles pane wiring (M4).

## Acceptance

- [x] All existing tests pass after the migration (365 today; net delta should be 0 or slightly positive).
- [x] The field-test repro (lock → remote power-off → mouse → TV wakes) still works end-to-end on the LG OLED G4.
- [x] No retry-loop / verify logic in the `tisvlay-session` crate; that crate's role is reduced to "read profile, call dispatch_action".
- [x] System-event dispatch (`on_wake`, `on_startup` etc.) goes through the same helper, so configuring `on_wake = screen_on` would also get reliable behavior.
- [x] `tisvlay-session/src/lib.rs` net LOC drops; `crates/tisvlayd/src/tv_manager.rs` (or sibling) net LOC grows by roughly the moved+adapted amount.
