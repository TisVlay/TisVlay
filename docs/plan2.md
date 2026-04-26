# Follow-up plan — regression test + coalescer factor-out

The `RefreshTrigger` refactor is done (`tisvlay_core::RefreshTrigger`,
both watch_state loops use it). This file scopes the two remaining
follow-ups from the prior plan: **(A)** a regression test for the
StateWatcher consumer contract, and **(C)** factoring the per-key
coalescing pattern shared between the tray and GUI.

## What just shipped

`refactor(core): factor StateWatcher consumer pattern into RefreshTrigger`
— `RefreshTrigger` lives in `crates/tisvlay-core/src/dbus_client.rs`
(end of file, after `impl StateWatcher`). Re-exported from
`crates/tisvlay-core/src/lib.rs`. Tray and GUI `watch_state` shrink to
small loops that delegate spawn/dedupe to `trigger.fire(...)`.

Verified live: GUI slider flood for ~30s does not wedge tray or GUI.

## A. Regression test for the consumer-pattern contract

**Goal.** Catch any future regression where someone refactors
`RefreshTrigger::fire` (or a hand-rolled equivalent) into the broken
"await refresh inline" shape that filled the kernel recv buffer and
hung the tray.

**Risk.** The bug is timing-dependent. Reproducing the kernel-buffer
fill in a unit test is hard — it needs enough signal volume + low
enough scheduling priority that the consumer falls behind. May not
deterministically catch the bug. Worth a small spike to confirm
before committing.

**Sequence.**

1. **Spike: confirm `MockHandle` can drive rapid state pushes.** Read
   `crates/tisvlay-mock/src/handle.rs` and `state.rs` to find the
   API. Look for something like `push_state(TvState)` or
   `set_volume(...)`. The test needs a way to fire many
   `PropertiesChanged` signals back-to-back without going through the
   full backend round-trip.
2. **If the spike confirms feasibility**, write
   `crates/tisvlayd/tests/refresh_trigger_under_flood.rs`:
   - Boot `TestDaemon::with_one_mock_tv("flood_target")`.
   - Spawn a `watch_state`-style consumer that uses
     `RefreshTrigger::fire` to call `daemon.list_tvs()` on every event.
     This consumer shares the `Arc<DaemonClient>` with the test's
     "command issuer" — mimics tray/GUI's structure.
   - Drive a flood: 1000 rapid state changes via `MockHandle`.
   - Concurrently call `client.list_tvs()` (or any method) on the
     same client with `tokio::time::timeout(Duration::from_secs(2),
     ...)`.
   - Assert `Ok(_)` within timeout.
3. **If the spike fails** (mock doesn't support flooding, or the test
   is too weak to repro the bug), drop (A) and document the gap. The
   `RefreshTrigger` docstring already preserves the bug history.

**File to create:** `crates/tisvlayd/tests/refresh_trigger_under_flood.rs`
(feature-gated `#![cfg(feature = "mock")]` like other test files).

**Estimated effort.** 2-4 hours including the spike. Could be < 1 hour
if `MockHandle` already exposes a clean push API.

## C. Coalescer factor-out

**Goal.** The tray and GUI both coalesce rapid user input before
issuing daemon writes, but with different merge semantics:

- **GUI** (`tisvlay-gui/src/main.rs`, `coalesce_send` helper): per-TV
  last-wins for absolute slider values. `(tv_id, latest_value) →
  daemon.set_volume`.
- **Tray** (`tisvlay-tray/src/main.rs`, inside `process_commands` for
  `TrayCommand::ScrollBy`): sum-merge for relative scroll deltas.
  `(tv_id, target, sum_of_deltas) → daemon.set_volume`.

Both could share a generic `Coalescer<K, V>` in `tisvlay-core` with a
configurable merge function.

**Risk.** The tray's coalescing is **interleaved with command
dispatch** (`while let Some(cmd) = cmd_rx.recv().await { ... }` then
`while let Ok(other) = cmd_rx.try_recv() { coalesce or requeue }`). It
isn't a drop-in helper — extracting it changes the shape of
`process_commands`. The GUI's `coalesce_send` is also already working
correctly. Net win is medium dedup + one shared primitive vs. moderate
refactor risk.

**Recommended only if:** another consumer needs coalescing (e.g., the
upcoming Profiles pane firing rapid `set_user_profile` calls during a
dropdown scroll), making the third call site that justifies the
generic abstraction. Two call sites is the minimum where dedup pays
off; three makes it clearly worth the genericization.

**Proposed API.**

    pub struct Coalescer<K, V> {
        senders: Mutex<HashMap<K, mpsc::UnboundedSender<V>>>,
    }

    impl<K, V> Coalescer<K, V>
    where
        K: Hash + Eq + Clone + Send + 'static,
        V: Send + 'static,
    {
        pub fn new() -> Self { ... }

        // `merge(old, new) -> next`. Last-wins: `|_, new| new`.
        // Sum:                              `|old, new| old + new` (where V: Add).
        pub fn fire<Merge, Apply, Fut>(
            &self,
            key: K,
            value: V,
            merge: Merge,
            apply: Apply,
        )
        where
            Merge: Fn(V, V) -> V + Send + Sync + Clone + 'static,
            Apply: Fn(K, V) -> Fut + Send + Sync + Clone + 'static,
            Fut: Future<Output = ()> + Send;
    }

The worker per key drains its mpsc with `recv()`, then `try_recv()`s
in a loop applying `merge(latest, next)` to fold values, then calls
`apply(key, latest)`.

**Sequence.**

1. **Defer until a third call site exists.** Don't preemptively
   refactor.
2. **When trigger arrives**: implement `Coalescer<K, V>` in
   `crates/tisvlay-core/src/dbus_client.rs` (or a fresh module — see
   open question below).
3. **Migrate GUI's `coalesce_send`** to use it (last-wins). One
   commit. ~30-line diff.
4. **Migrate tray's `ScrollBy` coalescing**: this is the bigger
   restructure of `process_commands`. Keep the `requeue_tx`
   non-scroll-command pattern; route scroll commands through the
   coalescer instead of the inline `try_recv` loop. Separate commit.
5. **Build / lint / test / live verify.**

**Estimated effort.** 1 day for both migrations, including the live
verify. Single-day work but worth scoping by call-site count.

## Open question

Should `Coalescer<K, V>` and `RefreshTrigger` live in
`dbus_client.rs`, or move into a new `tisvlay-core/src/watcher.rs`
(or `concurrency.rs`)? Both are tokio-spawn helpers tightly bound to
the StateWatcher consumer pattern, but neither depends on `dbus_client`
internals. A new module would be cleaner but adds a re-export step.
Defer the decision to whoever ships (C) — by then we'll have two
helpers in the same idiom and a clearer signal whether they deserve
their own module.

## Files affected by these follow-ups

**(A)**
- `crates/tisvlayd/tests/refresh_trigger_under_flood.rs` (new, ~80 lines)

**(C)** (deferred until a third coalescer call site exists)
- `crates/tisvlay-core/src/dbus_client.rs` (+ generic `Coalescer<K, V>`)
- `crates/tisvlay-core/src/lib.rs` (re-export)
- `crates/tisvlay-gui/src/main.rs` (drop `coalesce_send`, use `Coalescer`)
- `crates/tisvlay-tray/src/main.rs` (restructure `process_commands`)

## Recommendation

- **Skip (A)** for now. Low ROI given timing nondeterminism; the
  `RefreshTrigger` docstring already documents the bug. If a future
  consumer regression slips through, revisit with the full repro in
  hand (which informs a much sharper test).
- **Defer (C)** until a third coalescer call site materializes
  (likely during M3.5/M4 if profile dropdowns fire fast enough to
  saturate the daemon).
- **Move on to broader plan**: M3.5 (daemon profile CRUD over D-Bus,
  unblocks GUI Profiles pane), then M4 (Profiles pane wiring), then
  M5 (Session pane), then M6 (packaging). See [docs/plan.md](plan.md).

If the user disagrees, easy reversals: do (A) first as a small
experiment (1 hour spike to confirm `MockHandle` viability before
committing to the test), or do (C) preemptively (1 day) to land it
before the third call site forces it.
