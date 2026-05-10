# Per-TV lifecycle correctness: defensive register-refresh + drain semantics

> **Status:** drafted 2026-05-10, revised 2026-05-10 mid-implementation. Two follow-up observations from the [GUI mock-pair flow + reload-path PR](../../PR.md) review, addressed together because they share the "per-TV lifecycle consistency" theme. Phase 1 ensures `register_tv_interfaces` always produces fresh per-TV state by unregistering any pre-existing path-collision first — closing a silent-state-staleness gap in zbus 5.x's `ObjectServer::at` semantics. Phase 2 (mostly verification) confirms that the existing per-TV `Mutex<ManagedTv>` already provides drain semantics for in-flight `Tv1.*` calls during removal, captures that contract in code comments, and locks it down with a regression test.
>
> **Mid-implementation reframing**: the plan initially proposed typed `DaemonError` variants and an AddTv rollback to surface "partial register failures" on `Manager1.AddTv` and `Manager1.ReloadConfig`. Hands-on code reading of zbus 5.14.0's `ObjectServer::at` revealed that **path collisions return `Ok(false)`, not `Err`** ([`object_server/mod.rs:118-119`](https://docs.rs/zbus/5.14.0/zbus/object_server/struct.ObjectServer.html#method.at)), so `register_tv_interfaces`'s `.await?` never short-circuits on the collision case it was assumed to handle. The "failure" mode is silent state staleness — a pre-existing registration keeps serving with old field values when a fresh `register` is attempted. The right fix is **defensive `unregister` before `register`** (the only way to flush stale state), not typed errors or rollback paths that would be dead code in practice. Plan scope narrowed accordingly.

## Context

Two observations were deliberately deferred from the prior PR's review (see PR body's "Follow-up" section) and surfaced for fix here:

1. **`apply_reload_diff`'s register call doesn't refresh stale per-TV interface state.** Same shape as `Manager1.AddTv` today. If a pre-existing registration at the same path is present (extremely rare — would require a prior failed AddTv that left interfaces registered but no record in `tv_manager`), the fresh register silently does nothing and the stale interface keeps serving. **Reframed mid-implementation** — see boxed status note above.
2. **Unregister-during-in-flight-`Tv1.*`-call race.** Pre-existing pattern in `Tv1.Remove` (and now `apply_reload_diff` via the reload-path fix). Pre-PR review framed this as "in-flight call could see stale state and return Ok"; post-research it turns out the existing per-TV `Mutex<ManagedTv>` already serializes correctly and the observable behavior is coherent in every sub-case.

Research findings ([Agent A internal trace, Agent B zbus semantics, Agent C industry convention survey, all 2026-05-10]) plus mid-implementation zbus source dive:

- **`ObjectServer::at` returns `Ok(bool)` on already-registered.** The function's docstring at [`object_server/mod.rs:118`](https://docs.rs/zbus/5.14.0/zbus/object_server/struct.ObjectServer.html#method.at) reads "If the interface already exists at this path, returns false." `register_tv_interfaces` uses `.await?` on each of seven `at` calls and discards the bool, so collision returns `Ok(())` from the wrapping function with the pre-existing registration intact. **There is no `Err` path for collision.** This reshapes Phase 1 — see the boxed status note.
- **In-flight call mechanic is not actually racy in this codebase.** Three sub-cases — (a) call hasn't reached `get_tv` yet, (b) got the `Arc<Mutex<ManagedTv>>` but awaiting the inner Mutex, (c) holding the Mutex during SSAP write — all produce coherent results. (a) yields `TvNotFound`, (b) yields `NotConnected` after removal closes the connection, (c) blocks the removal on the Mutex until the in-flight write completes. No stale-Ok scenario exists.
- **Industry convention surveys** (NM, BlueZ, iwd, udisks2, systemd partial-failure handling; BlueZ/NM cancel-with-error on remove) are still in the file's history for posterity but no longer drive Phase 1's design. The defensive-unregister approach is a zbus-specific workaround for a zbus-specific contract, not an industry-pattern choice. Phase 2 still leans on the in-flight-call research.
- **zbus 5.x has no drain primitive.** `ObjectServer::remove` mutates a `BTreeMap` under the root `RwLock` and returns; in-flight handlers complete against a previously-cloned `Arc<RwLock<dyn Interface>>`. Idiomatic workarounds (sentinel `AtomicBool`, take-write-lock-first) exist but aren't blessed. Phase 2 documents that our per-TV `Mutex<ManagedTv>` is the drain layer.

## Design decisions

### DR1: Defensive `unregister` before `register` at every register site

The bug surface is silent state staleness, not partial failure. `register_tv_interfaces` cannot return `Err` on path collision because zbus's `at` returns `Ok(false)` and the wrapping function discards the bool. So a fresh `register` against a path that has a stale registration silently does nothing — the pre-existing interfaces keep serving with their old field values.

The fix is `unregister_tv_interfaces` (which is infallible by construction — swallows per-interface errors and always returns `Ok`) right before every `register_tv_interfaces` call site. This flushes any stale interfaces so the new register's `state` parameter actually takes effect. Cheap when there's nothing to flush; correct when there is.

Applied at three call sites:

- `apply_reload_diff` in `dbus/setup.rs` — already did unregister on the `removed` half; now also unregisters before register on the `added` half.
- `Manager1.AddTv` in `dbus/manager.rs` — defensive unregister before the post-pair register call.
- `build_dbus_connection`'s startup loop in `dbus/setup.rs` — first registration after process start, no stale state possible, so no defensive unregister needed here.

No new `DaemonError` variants. No rollback paths. The original plan's "RegisterInterfacesFailed" and "ReloadPartialFailure" variants would have been dead code given zbus's actual contract.

### DR2: No rollback in AddTv

Originally proposed as a deviation from industry convention because of the "no per-TV path to call Tv1.Remove on after register failure" asymmetry. With zbus's actual contract, `register_tv_interfaces` doesn't fail on collision, so there's nothing to roll back from. AddTv keeps its existing `warn!`-and-continue posture for the residual failure modes (connection-level errors from broken D-Bus, which are catastrophic for the daemon and unrecoverable at the per-call level anyway).

If a future zbus version *does* change `at`'s contract to return `Err` on collision, revisit. Until then, AddTv's failure surface is unchanged from today.

### DR3: Unregister-during-in-flight Tv1.* call → preserve existing Mutex-drain (not upgrade to cancel-with-error)

The existing per-TV `Mutex<ManagedTv>` *is* our drain layer. Sub-case (c) (in-flight call holding the Mutex during SSAP work) blocks the remove path on the same Mutex; the in-flight write completes; the close+unregister proceeds afterward. Sub-cases (a) and (b) produce explicit error replies (`TvNotFound`, `NotConnected`). Every observable outcome is coherent.

Upgrading to BlueZ's cancel-with-error (sentinel `AtomicBool` checked at the top of every `Tv1.*` handler, set during `apply_reload_diff` / `Tv1.Remove`, in-flight handlers bail with a synthetic `Tv1.Cancelled` error) would add per-handler complexity for a behavioral change of debatable value: today the user's `SetVolume` succeeds against a TV being removed, tomorrow it would fail with "TV is being removed" — but their TV is already going away regardless. Net UX is similar; the cost is real.

Skip the upgrade. **Document** the Mutex-drain pattern in code (comments at `tv_manager::remove_tv` and `apply_reload_diff`), and **regression-test** it so a future refactor doesn't accidentally drop the Mutex-held-across-await pattern that makes it work.

### DR4: Scope → one plan, two phases, one PR

Both phases touch overlapping surface (`tv_manager.rs`, `dbus/manager.rs`, `dbus/setup.rs`). Splitting would create dependent PRs. Phases are small enough that one PR with separate commits per phase is reviewable.

### DR5: Other lifecycle paths

- **`Tv1.Reconnect`** — closes the connection, reopens it. No register/unregister; the path stays. Same Mutex-drain applies. **Audit only**, no fix expected.
- **`Tv1.UpdateConfig` with vendor change** — closes the connection, updates the in-memory config, reconnects. Interfaces registered are the same regardless of vendor (capability differences are exposed via the `Capabilities` property, not the interface set), so no re-registration is needed. **Audit only**, no fix expected.
- **SIGHUP reload** — already routes through the same `apply_reload_diff` helper (per the prior PR), so it inherits the defensive-unregister-before-register treatment for free. No SIGHUP-handler change needed.

## Source-of-truth audit

### Phase 1: Defensive register-refresh

| Site | Today | Should be |
|---|---|---|
| `crates/tisvlayd/src/dbus/setup.rs::apply_reload_diff` | Per-added-TV `register_tv_interfaces` with no prior unregister | Add `let _ = unregister_tv_interfaces(...)` before each `register_tv_interfaces` on the `added` half. Removed-half unregister logic unchanged. Function still returns `()`. |
| `crates/tisvlayd/src/dbus/manager.rs::add_tv` (~L256-L266) | `register_tv_interfaces(...)` with no prior unregister | Add `let _ = unregister_tv_interfaces(...)` before the post-pair register. AddTv still `warn!`s on register error and continues — no rollback, no typed-error variant. |
| New regression test | n/a | `tests/register_tv_interfaces_collision.rs` locking down the contracts: (a) duplicate register returns `Ok(())` silently — the failure mode the defensive unregister addresses, (b) unregister followed by register succeeds — the foundation of the defensive pattern. |

### Phase 2: Drain semantics

| Site | Today | Should be |
|---|---|---|
| `crates/tisvlayd/src/tv_manager.rs::remove_tv` (~L1656) | No comment on Mutex-drain semantics | Add a brief block comment documenting the per-TV-`Mutex` serialization with in-flight `Tv1.*` calls: case (a) `TvNotFound`, case (b) `NotConnected`, case (c) blocks until in-flight completes. |
| `crates/tisvlayd/src/dbus/setup.rs::apply_reload_diff` | Same | Cross-reference comment pointing at `remove_tv`'s docstring. |
| New `crates/tisvlayd/tests/in_flight_call_during_remove.rs` | Doesn't exist | Regression test verifying the three sub-cases produce the documented results. Mock backend's slow-write injection (or a new test hook) drives the timing. |

## Phase 1 steps — defensive register-refresh

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Refactor `apply_reload_diff` in `dbus/setup.rs` to call `let _ = unregister_tv_interfaces(...)` before each `register_tv_interfaces` on the `added` half. Function-level comment documents the zbus `Ok(false)`-on-collision rationale. | done | Removed-half unregister logic is unchanged (already infallible-by-construction). Return type stays `()`. |
| 2 | Update `dbus/manager.rs::add_tv` similarly: defensive `unregister_tv_interfaces` before the post-pair register. Keep the existing `warn!`-and-continue posture for register failures (which are connection-level only, not collision-based). | done | No rollback, no new variants. |
| 3 | Regression test `crates/tisvlayd/tests/register_tv_interfaces_collision.rs`: lock down the two contracts the defensive pattern relies on — (a) `register_tv_interfaces` on an already-registered path returns `Ok(())` silently (the failure mode), (b) `unregister_tv_interfaces` followed by `register_tv_interfaces` succeeds (the foundation). Both tests use `TestDaemon::with_one_mock_tv` since `build_dbus_connection`'s startup loop already registers a path for re-registration to land on. | done | Pre-revision plan proposed asserting `Err(AddressInUse)` here — zbus's actual contract makes that wrong. The reshaped tests document what `at` actually does. |
| 4 | `prek run -a` end-to-end gate for Phase 1. | todo | Per `feedback_lint_and_test_before_commit.md`. |

## Phase 2 steps — drain semantics documentation + regression test

| # | Step | Status | Notes |
|---|---|---|---|
| 5 | Audit `Tv1.Reconnect` and `Tv1.UpdateConfig` (vendor-change branch) for analogous lifecycle exposure. Confirm no register/unregister is needed; capture the audit conclusion in a code comment near each handler. | todo | DR5 work; expected to be a no-op fix-wise. |
| 6 | Add a block comment on `tv_manager::remove_tv` documenting the per-TV-`Mutex` drain semantics for in-flight `Tv1.*` calls (the three sub-cases and their respective behaviors). | todo | The contract today is implicit; this makes it explicit so a future refactor doesn't drop it. |
| 7 | Cross-reference comment on `apply_reload_diff` pointing at the `remove_tv` docstring. | todo | Same drain semantics apply to reload's removal half. |
| 8 | New regression test `crates/tisvlayd/tests/in_flight_call_during_remove.rs`. Drive the three sub-cases deterministically using the mock backend's slow-write injection (or a new test seam in the mock connector that blocks `set_volume` on a tokio oneshot until the test signals). Assert: case (a) → `TvNotFound`; case (b) → `NotConnected`; case (c) → in-flight write returns `Ok` and removal completes after. | todo | Determinism via signaling channels, not real-time sleeps; per `concerns/session-event-tests-real-time-sleeps.md` we want to avoid that pattern. |
| 9 | `prek run -a` and `just check` end-to-end gate. | todo | |
| 10 | Smoke test on a Wayland session: pair a mock TV, edit config to remove it AND add a different one, click Reload — verify the GUI shows the swap correctly. | todo (user) | The original step also called for an induced-failure smoke test, but with the typed-error path dropped that scenario isn't observable from the GUI. The regression test in step 8 covers the drain-semantics contract. |

## Risks / unknowns

- **Phase 1 reframing leaves the original observation #2 ("partial-failure visibility") in a different light**: the user-facing concern from the PR review ("if register_tv_interfaces fails, the daemon's in-memory and on-bus state can drift") is now addressed by the defensive unregister rather than by typed-error propagation. The drift no longer happens, so there's nothing left to propagate. If the zbus contract ever changes (`at` returns `Err` on collision in some future version), the original typed-error approach becomes relevant again — file that as a watch item, not a blocker.
- **Mock backend's slow-write injection availability** (Phase 2 regression test): the mock has `simulate_user_*` for pairing but I haven't verified slow-write infrastructure for `set_volume` etc. May need to add a new injection point. Cheap if so.

## Out of scope

- **Typed `DaemonError` variants for register failure** (`RegisterInterfacesFailed`, `ReloadPartialFailure`). Dropped during the mid-implementation reframing: zbus's `at` returns `Ok(false)` not `Err` on collision, so the register failure they were meant to surface doesn't exist as an Err path. The defensive unregister addresses the actual bug (silent state staleness) without needing new variants. If a future zbus contract change makes collision return `Err`, revisit.
- **AddTv rollback on register failure.** Same reframing: there's no register failure to roll back from in practice. AddTv keeps `warn!`-and-continue for the residual connection-level failure modes.
- **GUI/Tray client error handling for partial-register failures.** Originally in Phase 1; dropped because the typed errors that would have driven the new UI text don't exist. The current opaque-error path is unchanged.
- **Cancel-with-error upgrade for in-flight Tv1.* during removal.** Skipped per design decision DR3. If we ever observe a real-world UX issue from the "in-flight write succeeds then TV vanishes" pattern, revisit.
- **`Manager1.ForceRemoveTv(tv_id)` escape hatch.** Originally cited as something the AddTv rollback made unnecessary; with the rollback dropped, the question is moot — the original need was driven by an imagined register-failure-with-no-cleanup-path scenario that doesn't actually happen.
- **Atomic add via "register first, pair second".** Considered briefly: register `Tv1.*` interfaces with placeholder/zeroed state before pair, update them after. Cleaner failure semantics but a much bigger refactor (pair_tv currently produces the connection that registration's `initial_state` reads from). Defer indefinitely unless a stronger reason emerges.
- **Drain-style remove that waits for *all* in-flight calls** (not just the one holding the Mutex). zbus has no primitive for this; we'd have to implement a per-path counter. The Mutex-drain handles the only case where it functionally matters (case c).
- **Profile/system-config CRUD partial-failure semantics** (M3.5 / M4 in `docs/plan.md`). Same lifecycle-correctness theme, but the surface is different (profiles aren't `Tv1.*` paths). Address when those daemon methods land.

## References

- [`PR.md`](../../PR.md) — the prior PR's "Follow-up" section called out these two observations.
- [zbus 5.14.0 `ObjectServer::at`](https://docs.rs/zbus/5.14.0/zbus/object_server/struct.ObjectServer.html#method.at) — the docstring "If the interface already exists at this path, returns false" that reframed Phase 1 mid-implementation. The actual contract is `Result<bool>`, not the `Result<()>` with `Err` on collision that the original plan assumed.
- [`crates/tisvlayd/src/tv_manager.rs::reload_config`](../../crates/tisvlayd/src/tv_manager.rs) — `ReloadDiff`-returning reload body that Phase 1 builds on.
- [`crates/tisvlayd/src/dbus/setup.rs::apply_reload_diff`](../../crates/tisvlayd/src/dbus/setup.rs) — the helper that gets the defensive `unregister` before `register` on the `added` half.
- [`crates/tisvlayd/src/dbus/manager.rs::add_tv`](../../crates/tisvlayd/src/dbus/manager.rs) — gets the defensive `unregister` before its post-pair `register`.
- [`crates/tisvlayd/src/dbus/tv.rs::register_tv_interfaces` / `unregister_tv_interfaces`](../../crates/tisvlayd/src/dbus/tv.rs) — the per-interface helpers.
- [`crates/tisvlayd/tests/register_tv_interfaces_collision.rs`](../../crates/tisvlayd/tests/register_tv_interfaces_collision.rs) — Phase 1 regression test locking down the foundation contracts.
- [`docs/concerns/session-event-tests-real-time-sleeps.md`](../concerns/session-event-tests-real-time-sleeps.md) — the "no real-time sleeps in tests" guidance Phase 2's regression test must respect.
- [zbus 5.x `ObjectServer`](https://docs.rs/zbus/latest/zbus/object_server/struct.ObjectServer.html) — confirmed no drain primitive exists, validating Phase 2's "use the existing Mutex" approach.
