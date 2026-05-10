# Per-TV lifecycle correctness: partial-failure visibility + drain semantics

> **Status:** drafted 2026-05-10, not started. Two follow-up observations from the [GUI mock-pair flow + reload-path PR](../../PR.md) review, addressed together because they share the "per-TV lifecycle consistency" theme. Phase 1 (the bigger fix) gives the daemon and its clients a typed view of partial-register failures across `Manager1.AddTv` and `Manager1.ReloadConfig`. Phase 2 (mostly verification) confirms that the existing per-TV `Mutex<ManagedTv>` already provides drain semantics for in-flight `Tv1.*` calls during removal, captures that contract in code comments, and locks it down with a regression test.

## Context

Two observations were deliberately deferred from the prior PR's review (see PR body's "Follow-up" section) and surfaced for fix here:

1. **`apply_reload_diff` per-TV register failures are logged, not propagated.** Same shape as `Manager1.AddTv` today — both `warn!` and continue when `register_tv_interfaces` fails. The on-bus and in-memory state can drift silently when the rare failure (path-collision from a stale prior cycle) fires.
2. **Unregister-during-in-flight-`Tv1.*`-call race.** Pre-existing pattern in `Tv1.Remove` (and now `apply_reload_diff` via the reload-path fix). Pre-PR review framed this as "in-flight call could see stale state and return Ok"; post-research it turns out the existing per-TV `Mutex<ManagedTv>` already serializes correctly and the observable behavior is coherent in every sub-case.

Research findings ([Agent A internal trace, Agent B zbus semantics, Agent C industry convention survey, all 2026-05-10]):

- **In-flight call mechanic is not actually racy in this codebase.** Three sub-cases — (a) call hasn't reached `get_tv` yet, (b) got the `Arc<Mutex<ManagedTv>>` but awaiting the inner Mutex, (c) holding the Mutex during SSAP write — all produce coherent results. (a) yields `TvNotFound`, (b) yields `NotConnected` after removal closes the connection, (c) blocks the removal on the Mutex until the in-flight write completes. No stale-Ok scenario exists.
- **`register_tv_interfaces` failure modes are narrow.** Realistically only `zbus::Error::AddressInUse` (path already registered). All `tv_manager.*` queries used during registration have `unwrap_or_*` defaults, so they're infallible. The failure window is "stale state from a prior failed AddTv cycle" — defensive `unregister` before `register` collapses it.
- **Industry convention for partial-add failure: propagate, not roll back.** NetworkManager (`AddConnection`), BlueZ (`Pair`), iwd (`Connect`), udisks2 (`LoopSetup`), systemd (`StartUnit`) all propagate. None silently log+return Ok. Rollback exists only as opt-in (NM's `volatile`) or via separate transactional API (NM checkpoints). **However**, this codebase's asymmetry forces a different call for AddTv specifically: see "Design decisions" below.
- **Industry convention for unregister-during-in-flight: cancel-with-error.** BlueZ explicitly replies to pending Connect calls with `ERR_BREDR_CONN_CANCELED` before unregistering; NM tears down `priv->async_op_lst_head` with appropriate errors during deactivation. **However**, our existing Mutex-drain produces correct results without this complexity — see "Design decisions".
- **zbus 5.x has no drain primitive.** `ObjectServer::remove` mutates a `BTreeMap` under the root `RwLock` and returns; in-flight handlers complete against a previously-cloned `Arc<RwLock<dyn Interface>>`. Idiomatic workarounds (sentinel `AtomicBool`, take-write-lock-first) exist but aren't blessed.

## Design decisions

### Q1: AddTv on register failure → rollback (not propagate)

Industry convention is propagate-as-Err. **We deviate** because of an asymmetry: BlueZ's `Pair` failure leaves a `Device1` object on the bus (the Pair-target was discovered first via `Adapter.StartDiscovery`), so the client has a `Device1.RemoveDevice` path to call for cleanup. Our `Manager1.AddTv` registration is the *only* place a `/org/tisvlay/Tv/<UUID>` path is created. If register fails after pair succeeded, the client has *no per-TV path* to call `Tv1.Remove` on — the cleanup path doesn't exist until register succeeds. Propagating Err with no cleanup affordance leaves the user stuck with a TV in `tv_manager` + persisted on disk + invisible on the bus.

Rather than introduce a `Manager1.ForceRemoveTv(tv_id)` escape hatch, roll back: on `register_tv_interfaces` failure, undo `pair_tv` (call `manager.remove_tv(&tv_id).await`, which both drops the in-memory entry + persists the removal to config), close the connection, then return `Err(DaemonError::RegisterInterfacesFailed { tv_id, source })`. The client sees a clean failure with no orphan state to clean up.

This is morally equivalent to NM's opt-in `volatile` mode, applied unconditionally because our register isn't a separate user-controllable step.

### Q2: ReloadConfig partial failure → propagate (not roll back)

Reload reads from disk, which is the source of truth. Rolling back a per-TV register failure would mean reverting the in-memory state to "this TV not added" — but the on-disk config still has the entry, and the *next* reload would reattempt and fail again. So rollback would just hide the problem.

Propagate as `DaemonError::ReloadPartialFailure { failed_ids: Vec<String> }`. Successful adds/removes stay applied; failed adds get a `warn!` log per-TV (preserved) plus an aggregate error returned to the client carrying the list of TV ids that didn't make it to the bus. Client UX (GUI/Tray) can surface a "config reloaded, but N TVs failed to register" notice.

### Q3: Unregister-during-in-flight Tv1.* call → preserve existing Mutex-drain (not upgrade to cancel-with-error)

The existing per-TV `Mutex<ManagedTv>` *is* our drain layer. Sub-case (c) (in-flight call holding the Mutex during SSAP work) blocks the remove path on the same Mutex; the in-flight write completes; the close+unregister proceeds afterward. Sub-cases (a) and (b) produce explicit error replies (`TvNotFound`, `NotConnected`). Every observable outcome is coherent.

Upgrading to BlueZ's cancel-with-error (sentinel `AtomicBool` checked at the top of every `Tv1.*` handler, set during `apply_reload_diff` / `Tv1.Remove`, in-flight handlers bail with a synthetic `Tv1.Cancelled` error) would add per-handler complexity for a behavioral change of debatable value: today the user's `SetVolume` succeeds against a TV being removed, tomorrow it would fail with "TV is being removed" — but their TV is already going away regardless. Net UX is similar; the cost is real.

Skip the upgrade. **Document** the Mutex-drain pattern in code (comments at `tv_manager::remove_tv` and `apply_reload_diff`), and **regression-test** it so a future refactor doesn't accidentally drop the Mutex-held-across-await pattern that makes it work.

### Q4: Scope → one plan, two phases, one PR

Both phases touch overlapping surface (`tv_manager.rs`, `dbus/manager.rs`, `dbus/setup.rs`, the `DaemonError` enum). Splitting would create dependent PRs. Phases are small enough that one PR with separate commits per phase is reviewable.

### Q5: Other lifecycle paths

- **`Tv1.Reconnect`** — closes the connection, reopens it. No register/unregister; the path stays. Same Mutex-drain applies. **Audit only**, no fix expected.
- **`Tv1.UpdateConfig` with vendor change** — closes the connection, updates the in-memory config, reconnects. Interfaces registered are the same regardless of vendor (capability differences are exposed via the `Capabilities` property, not the interface set), so no re-registration is needed. **Audit only**, no fix expected.
- **SIGHUP reload** — already routes through the same `apply_reload_diff` helper (per the prior PR), so it inherits Phase 1's typed-error treatment for free. The SIGHUP handler doesn't have a D-Bus client to return errors to; it just logs. **Update the SIGHUP arm's `match` to log per-TV failures by id** rather than the aggregate-only signal we have today.

## Source-of-truth audit

### Phase 1: Partial-failure visibility

| Site | Today | Should be |
|---|---|---|
| `crates/tisvlayd/src/dbus/manager.rs::add_tv` (~L256-L266) | `if let Err(e) = register_tv_interfaces(...) { warn!(...); }`; returns `Ok((path, tv_id))` regardless | Defensive `unregister_tv_interfaces` first, then `register_tv_interfaces`. On register-still-fails: `manager.remove_tv(&tv_id).await` to roll back; return `Err(DaemonError::RegisterInterfacesFailed { tv_id, source })`. |
| `crates/tisvlayd/src/dbus/setup.rs::apply_reload_diff` | Per-TV `if let Err(e) { warn!(...); }`; returns `()` (no error type to begin with) | Change return type to `ReloadFailures { failed_added: Vec<(String, zbus::Error)> }` (an empty struct collapses to `Result<(), DaemonError>` at the dbus boundary). Defensive `unregister` before `register` per added TV. Collect failures rather than dropping them. |
| `crates/tisvlayd/src/dbus/manager.rs::reload_config` | Calls `apply_reload_diff` and returns `Ok(())` | Call `apply_reload_diff`, inspect the failure list, return `Err(DaemonError::ReloadPartialFailure { failed_ids })` if non-empty. |
| `crates/tisvlayd/src/main.rs::wait_for_signals` SIGHUP arm | `match manager.reload_config().await { Ok(diff) => apply_reload_diff(...) }` — no failure surface | Inspect the per-TV failure list from `apply_reload_diff` and `warn!` per failed id (no error to propagate; signal handler has no caller). |
| `crates/tisvlayd/src/dbus/mod.rs::DaemonError` | No partial-failure variants | Add `RegisterInterfacesFailed(String)` and `ReloadPartialFailure(String)` (with payloads carrying the tv_id and aggregate failed-id list respectively, formatted into the string per existing convention). |
| GUI client surface ([`crates/tisvlay-gui/src/lib.rs::on_request_pair`](../../crates/tisvlay-gui/src/lib.rs)) | Treats AddTv error as opaque `OperationFailed` | Pattern-match on the new typed variants and surface specific toast/message text. Same for the Reload sidebar path. |

### Phase 2: Drain semantics

| Site | Today | Should be |
|---|---|---|
| `crates/tisvlayd/src/tv_manager.rs::remove_tv` (~L1656) | No comment on Mutex-drain semantics | Add a brief block comment documenting the per-TV-`Mutex` serialization with in-flight `Tv1.*` calls: case (a) `TvNotFound`, case (b) `NotConnected`, case (c) blocks until in-flight completes. |
| `crates/tisvlayd/src/dbus/setup.rs::apply_reload_diff` | Same | Cross-reference comment pointing at `remove_tv`'s docstring. |
| New `crates/tisvlayd/tests/in_flight_call_during_remove.rs` | Doesn't exist | Regression test verifying the three sub-cases produce the documented results. Mock backend's slow-write injection (or a new test hook) drives the timing. |

## Phase 1 steps — partial-failure visibility

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Add `DaemonError::RegisterInterfacesFailed(String)` and `DaemonError::ReloadPartialFailure(String)` variants in `crates/tisvlayd/src/dbus/mod.rs`. Update the `From<…>` blocks if needed. | todo | Match existing variant style — single `String` payload, formatted at the call site. |
| 2 | Refactor `apply_reload_diff` in `dbus/setup.rs` to (a) try defensive `unregister_tv_interfaces` before `register_tv_interfaces` for each added id, (b) collect register failures into a `Vec<(String, zbus::Error)>`, (c) return that vec from the helper. | todo | Removed-side unregister logic stays unchanged (it's already infallible-by-construction). |
| 3 | Update `dbus/manager.rs::reload_config` to consume the failure vec and return `Err(DaemonError::ReloadPartialFailure(...))` if non-empty. Format the error message with the failed ids. | todo | Successful registers stay applied; the error signals the partial state without rolling back. |
| 4 | Update `main.rs::wait_for_signals` SIGHUP arm to log per-failed-id from the same vec. | todo | Signal handler has no caller to return errors to — `warn!` per id with the underlying zbus error. |
| 5 | Update `dbus/manager.rs::add_tv` to (a) try defensive `unregister_tv_interfaces` before `register_tv_interfaces`, (b) on register failure, call `manager.remove_tv(&tv_id).await` to roll back the pair, (c) return `Err(DaemonError::RegisterInterfacesFailed(...))`. | todo | The rollback is the asymmetry vs. industry convention — rationale captured in the design-decisions section above and inline at the call site. |
| 6 | New regression test `crates/tisvlayd/tests/add_tv_register_failure_rolls_back.rs`. Inject a register failure (simplest: pre-register the path before AddTv to force `AddressInUse`, or a feature-gated test seam in `register_tv_interfaces`) and assert AddTv returns `RegisterInterfacesFailed`, `manager.list_tv_ids()` doesn't contain the would-be tv, and the on-disk config is reverted. | todo | The test seam vs. pre-register choice depends on what's cleanest — pre-register is more realistic; test seam is more deterministic. |
| 7 | New regression test `crates/tisvlayd/tests/reload_partial_register_failure.rs`. Same shape but for `Manager1.ReloadConfig` with two TVs added in the new config, one of which has a colliding pre-registered path. Assert the unaffected TV registers cleanly, the affected one is reported in `ReloadPartialFailure`, and successful additions stay applied. | todo | Independent of test 6 in case the seam choice differs. |
| 8 | GUI client: pattern-match the new typed errors in `tisvlay-gui/src/lib.rs::on_request_pair` and the Reload sidebar handler. Surface user-facing messages: "Pairing succeeded but the TV couldn't be exposed on the system bus — try Pair again" for `RegisterInterfacesFailed`, "Reload completed but {N} TVs failed to register — see logs" for `ReloadPartialFailure`. | todo | Matching string forms — split on existing `: ` convention. |
| 9 | Tray client: similar handling on its Reload action. | todo | Tray's UI surface is narrower (status icons / tooltips) — at minimum log; a status-icon "warning" overlay would be nice if cheap. |
| 10 | `prek run -a` and `just check` end-to-end gate. | todo | Per `feedback_lint_and_test_before_commit.md`. |

## Phase 2 steps — drain semantics documentation + regression test

| # | Step | Status | Notes |
|---|---|---|---|
| 11 | Audit `Tv1.Reconnect` and `Tv1.UpdateConfig` (vendor-change branch) for analogous lifecycle exposure. Confirm no register/unregister is needed; capture the audit conclusion in a code comment near each handler. | todo | Q5 work; expected to be a no-op fix-wise. |
| 12 | Add a block comment on `tv_manager::remove_tv` documenting the per-TV-`Mutex` drain semantics for in-flight `Tv1.*` calls (the three sub-cases and their respective behaviors). | todo | The contract today is implicit; this makes it explicit so a future refactor doesn't drop it. |
| 13 | Cross-reference comment on `apply_reload_diff` pointing at the `remove_tv` docstring. | todo | Same drain semantics apply to reload's removal half. |
| 14 | New regression test `crates/tisvlayd/tests/in_flight_call_during_remove.rs`. Drive the three sub-cases deterministically using the mock backend's slow-write injection (or a new test seam in the mock connector that blocks `set_volume` on a tokio oneshot until the test signals). Assert: case (a) → `TvNotFound`; case (b) → `NotConnected`; case (c) → in-flight write returns `Ok` and removal completes after. | todo | Determinism via signaling channels, not real-time sleeps; per `concerns/session-event-tests-real-time-sleeps.md` we want to avoid that pattern. |
| 15 | `prek run -a` and `just check` end-to-end gate. | todo | |
| 16 | Smoke test on a Wayland session: pair a mock TV, edit config to remove it AND add a different one, click Reload — verify the GUI shows the swap correctly. Also: induce a register-failure scenario (pre-register the path with busctl?) and verify the error surface in the GUI. | todo (user) | The induced-failure smoke test is best-effort — if reproducing the failure path manually is awkward, the regression tests cover it. |

## Risks / unknowns

- **Test seam vs. pre-register for failure injection** (steps 6, 7, 14): pre-register via a second `object_server.at(...)` call from the test is more realistic and doesn't change product code. A test seam (`#[cfg(feature = "test-failure-injection")]` in `register_tv_interfaces`) is more deterministic but adds product-code surface. Pick during step 6; revisit if the test ends up flaky.
- **`manager.remove_tv` failure during AddTv rollback** (step 5): if the rollback itself fails, we have a worse state than just propagating. `remove_tv` is the same code path `Tv1.Remove` uses and is well-tested; failure would mean a deeper invariant violation. Log + wrap into the outer error; don't loop on retry.
- **Mock backend's slow-write injection availability** (step 14): the mock has `simulate_user_*` for pairing but I haven't verified slow-write infrastructure for `set_volume` etc. May need to add a new injection point. Cheap if so.
- **GUI/Tray client error mapping deferred work**: if the new typed errors go unmatched in clients, they fall through to the existing `OperationFailed` catch-all. Not a regression, just suboptimal UX. Steps 8-9 are nice-to-have for this PR; could split if scope creeps.

## Out of scope

- **Cancel-with-error upgrade for in-flight Tv1.* during removal.** Skipped per the design decision above. If we ever observe a real-world UX issue from the "in-flight write succeeds then TV vanishes" pattern, revisit.
- **`Manager1.ForceRemoveTv(tv_id)` escape hatch.** Made unnecessary by AddTv's rollback. If we ever need it for diagnostics or recovery from corrupted on-disk state, file as an idea.
- **Atomic add via "register first, pair second".** Considered briefly: register `Tv1.*` interfaces with placeholder/zeroed state before pair, update them after. Cleaner failure semantics but a much bigger refactor (pair_tv currently produces the connection that registration's `initial_state` reads from). Defer indefinitely unless a stronger reason emerges.
- **Drain-style remove that waits for *all* in-flight calls** (not just the one holding the Mutex). zbus has no primitive for this; we'd have to implement a per-path counter. The Mutex-drain handles the only case where it functionally matters (case c).
- **Profile/system-config CRUD partial-failure semantics** (M3.5 / M4 in `docs/plan.md`). Same lifecycle-correctness theme, but the surface is different (profiles aren't `Tv1.*` paths). Address when those daemon methods land.

## References

- [`PR.md`](../../PR.md) — the prior PR's "Follow-up" section called out these two observations.
- [`crates/tisvlayd/src/tv_manager.rs::reload_config`](../../crates/tisvlayd/src/tv_manager.rs) — `ReloadDiff`-returning reload body that Phase 1 builds on.
- [`crates/tisvlayd/src/dbus/setup.rs::apply_reload_diff`](../../crates/tisvlayd/src/dbus/setup.rs) — the helper that gets the failure-collection refactor.
- [`crates/tisvlayd/src/dbus/manager.rs::add_tv`](../../crates/tisvlayd/src/dbus/manager.rs) — gets the defensive-unregister + rollback treatment.
- [`crates/tisvlayd/src/dbus/tv.rs::register_tv_interfaces` / `unregister_tv_interfaces`](../../crates/tisvlayd/src/dbus/tv.rs) — the per-interface helpers.
- [`docs/concerns/session-event-tests-real-time-sleeps.md`](../concerns/session-event-tests-real-time-sleeps.md) — the "no real-time sleeps in tests" guidance Phase 2's regression test must respect.
- [BlueZ source — `src/device.c`](https://github.com/bluez/bluez/blob/master/src/device.c) lines 2047, 3346 — the cancel-with-error precedent we deviated from for Q3.
- [NetworkManager source — `nm-manager.c`](https://github.com/NetworkManager/NetworkManager/blob/main/src/core/nm-manager.c) line 918 — the `volatile` opt-in rollback pattern that motivated our unconditional AddTv rollback.
- [zbus 5.x `ObjectServer`](https://docs.rs/zbus/latest/zbus/object_server/struct.ObjectServer.html) — confirmed no drain primitive exists, validating Phase 2's "use the existing Mutex" approach.
