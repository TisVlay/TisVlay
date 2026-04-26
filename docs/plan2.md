# Refactor checkpoint — shared StateWatcher consumer + coalescer

Snapshot of the in-progress factor-out so a fresh session can pick up cleanly.
The user observed: "I'm seeing a lot of commonality between the tray and GUI,
like with coalescing and trigger_refresh stuff."

## What just shipped (already committed on `feat/gui`)

1. `fix(daemon): resync per-TV cached properties on ReloadConfig` — daemon
   re-resolves system profile on `Manager1.ReloadConfig` and `state_forwarder`
   propagates the change to the per-TV `Tv1` cached fields. Includes a
   `crates/tisvlayd/tests/reload_resolves_system_profile.rs` regression test.
2. `fix(gui): mutate TV model in place to preserve widget state across pushes` —
   `sync_vec_model` helper diffs the existing `VecModel<TvViewModel>` with
   `set_row_data`/`push`/`remove` instead of replacing the `ModelRc`, so the
   `for tv[i] in root.tvs: TvDetail` repeater preserves child widget instances
   (Slider drag mouse-capture, ScrollView offset, focus, etc.) across state
   pushes.
3. `fix: detach refresh from signal-drain loop in GUI and tray watchers` —
   both `watch_state` loops now spawn `refresh_state` on a separate task and
   dedupe via `AtomicBool`. Was: `loop { let event = watcher.next().await; refresh.await; }`. Awaiting refresh inline blocked the loop, halted zbus's
   signal_stream draining, filled the bounded internal channel, blocked the
   I/O task → method-call replies queued behind unread signal frames in the
   kernel recv buffer (verified: tray's recv-q hit 36 KB during a GUI slider
   flood, next ScrollBy hung indefinitely, daemon stayed responsive on a
   fresh `busctl` connection).
4. `feat(gui): coalesce per-TV slider drags to bound daemon queue depth` —
   per-`tv_id` last-wins coalescer in `tisvlay-gui/src/main.rs` for
   `set_volume` / `set_backlight` callbacks. mpsc + worker that drains-keeps-
   latest before each daemon round-trip.
5. `fix(gui): re-sync Slider thumb to model after user drag` — Slint's
   `Slider.value` is `in-out`; user drags write to it internally, permanently
   breaking the upstream `value: tv.volume` binding. Drive via
   `value <=> root.volume-display` against a local in-out property,
   re-establish on every `tv` change with `changed tv => { volume-display = tv.volume; }`.

The user repro for fixes 3-5: drag the GUI volume slider hard for ~30s while
the tray is open. Pre-fix: tray clicks/scrolls hung until tray restart. Post-
fix: tray (and GUI) stay responsive under sustained flood.

## Goal of this refactor (option B from the menu)

Both `tisvlay-tray/src/main.rs:184-264` (`watch_state`) and
`tisvlay-gui/src/main.rs:877-940` (`watch_state`) now contain near-identical
`trigger_refresh` boilerplate (closure + `AtomicBool` + spawn). The pattern is
the *contract* for consuming a `StateWatcher` correctly — anyone writing a
new consumer can easily get it wrong (and our git history shows we did).

Move the pattern into `tisvlay-core` so:
- New consumers naturally do the right thing.
- The contract is documented in one place with the bug history.
- Tray and GUI shrink to ~5-line `watch_state` functions.

## Proposed API: `RefreshTrigger`

Lives in `crates/tisvlay-core/src/dbus_client.rs` next to `StateWatcher`
(insert after the `impl StateWatcher` block ends at line 1684). Re-export
from `crates/tisvlay-core/src/lib.rs:114` alongside `StateWatcher`.

```rust
/// Spawn-with-dedupe trigger for refresh-style work driven by
/// [`StateWatcher`] events.
///
/// The canonical pattern for consuming a `StateWatcher` is:
///
/// ```ignore
/// let trigger = RefreshTrigger::new();
/// loop {
///     let Some(event) = watcher.next().await else { break };
///     match event {
///         StateEvent::DaemonStateChanged(_) | ... => {
///             let daemon = daemon.clone();
///             let session = session.clone();
///             let handle = handle.clone();
///             trigger.fire(move || {
///                 let daemon = daemon.clone();
///                 let session = session.clone();
///                 let handle = handle.clone();
///                 async move {
///                     refresh_state(&daemon, &session, &handle).await;
///                 }
///             });
///         }
///         StateEvent::DaemonDisappeared => { /* UI-specific */ }
///         /* … */
///     }
/// }
/// ```
///
/// `fire()` returns immediately. The watcher loop returns to
/// `watcher.next().await` without blocking, so zbus's signal_stream
/// stays drained regardless of refresh latency.
///
/// At most one refresh is in flight. Fires that arrive while a
/// refresh is running set a `dirty` flag; when the in-flight refresh
/// completes the worker checks the flag and runs another refresh
/// without releasing the in-flight slot. This guarantees trailing-
/// edge correctness — a flood ending exactly during a refresh still
/// captures the post-flood state.
///
/// # Why this matters
///
/// Awaiting refresh work inline in the `watcher.next()` loop body
/// halts signal_stream draining → zbus's bounded internal channel
/// fills → I/O task blocks pushing → connection stops reading from
/// the socket → method-call replies queue behind unread signal
/// frames in the kernel recv buffer. Subsequent client commands
/// hang waiting for replies that can't be read. See
/// commit XXXXXXX (the "detach refresh from signal-drain loop" fix)
/// for the full diagnosis.
pub struct RefreshTrigger {
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RefreshTrigger {
    pub fn new() -> Self {
        Self {
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark a refresh as needed; spawn a worker if one isn't already
    /// running. The worker loops as long as `dirty` is set, ensuring
    /// trailing-edge correctness.
    pub fn fire<F, Fut>(&self, refresh_fn: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        use std::sync::atomic::Ordering;
        self.dirty.store(true, Ordering::Relaxed);
        if self.in_flight.swap(true, Ordering::Relaxed) {
            return;
        }
        let in_flight = self.in_flight.clone();
        let dirty = self.dirty.clone();
        tokio::spawn(async move {
            // Loop until no fire happened during the last refresh —
            // captures any flood that ended exactly between iterations.
            while dirty.swap(false, Ordering::Relaxed) {
                refresh_fn().await;
            }
            in_flight.store(false, Ordering::Relaxed);
        });
    }
}

impl Default for RefreshTrigger {
    fn default() -> Self {
        Self::new()
    }
}
```

Note: `F: Fn` (not `FnOnce`) and `+ Sync` because the worker may call
`refresh_fn` multiple times across iterations and the closure may be
shared between threads. Closures that capture `Arc<T>` and clone inside
the body satisfy this; closures that *consume* captures don't compile,
which is the desired behavior.

## Sequence of edits

1. **Add `RefreshTrigger`** to `crates/tisvlay-core/src/dbus_client.rs`
   immediately after `impl StateWatcher { … }` (line ~1684).
2. **Re-export** by adding `RefreshTrigger` to the import list at
   `crates/tisvlay-core/src/lib.rs:114-116`.
3. **Refactor `tisvlay-tray/src/main.rs`** — replace the
   `let trigger_refresh = |daemon, session, handle, flag| { … }`
   closure (lines ~211-227) with `RefreshTrigger::new()` and `trigger.fire(|| async move { … })` calls in each match arm. Delete
   the `Arc<AtomicBool> refresh_in_flight` and the inline closure.
4. **Refactor `tisvlay-gui/src/main.rs`** — same change in its
   `watch_state` function (lines ~877-940).
5. **Build, lint, test** — `just check`. Both crates should compile;
   no behavioral test changes expected.
6. **Live verify** — restart daemon, tray, GUI; spam GUI slider hard;
   confirm both stay responsive (same repro as the original bug).
7. **Commit** as `refactor(core): factor StateWatcher consumer pattern
   into RefreshTrigger`.

After this commit, both `watch_state` functions become small enough
that the difference between them is purely UI dispatch (DaemonDisappeared
clears different state), not the spawn/dedupe machinery. Future
consumers can drop in the helper and write only the dispatch logic.

## Follow-ups (lower priority, scope these separately)

### A. Regression test for the consumer-pattern contract

A test in `crates/tisvlayd/tests/` that exercises the spawn-with-dedupe
pattern against a `TestDaemon` under simulated flood. Outline:

- Boot `TestDaemon::with_one_mock_tv("flood_target")`.
- Build a `DaemonClient` against the test bus, share between watcher
  and a "command issuer" (mimicking how tray/GUI use one client).
- Spawn a `watch_state`-style consumer using `RefreshTrigger`.
- Drive a flood via `MockHandle::push_state` (or whatever the mock's
  push API is — confirm by reading `crates/tisvlay-mock/src/handle.rs`).
  Push ~1000 state changes back-to-back.
- Concurrently issue `client.list_tvs()` with a 2-second
  `tokio::time::timeout`.
- Assert the call returns `Ok(_)` within the timeout.

The test is timing-dependent (kernel buffer fills are nondeterministic
under low load) but documents the contract — if someone refactors
`RefreshTrigger::fire` to await inline, the test times out.

Caveats: may need to actually saturate the daemon's event emitter to
reproduce; check if `MockHandle` supports rapid bulk state pushes. If
not, the test may be too weak to catch the original bug. Worth a small
spike before committing to it.

### C. Coalescer factor-out (separate, larger)

The GUI's `coalesce_send` helper (`tisvlay-gui/src/main.rs`, ~lines
1010-1050 after the recent commit — find via `grep -n 'fn coalesce_send'`)
is a per-key last-wins channel. The tray has a related but different
pattern in `tisvlay-tray/src/main.rs:304-352` (sum-merge of `ScrollBy`
deltas before the daemon call). They could share a generic primitive:

```rust
pub struct Coalescer<K, V> {
    senders: Mutex<HashMap<K, mpsc::UnboundedSender<V>>>,
    // …
}
impl<K, V> Coalescer<K, V> {
    pub fn fire<Merge, Apply, ApplyFut>(
        &self,
        key: K,
        value: V,
        merge: Merge,         // last-wins: |_, new| new   |   sum: |old, new| old + new
        apply: Apply,
    ) where Merge: Fn(V, V) -> V + …, Apply: Fn(K, V) -> ApplyFut + …
}
```

Lower priority because:
- The tray's coalescing is intermixed with command dispatch in
  `process_commands` — extracting it requires restructuring the
  command loop, not a drop-in replacement.
- The GUI's coalescer works correctly today; the only win is dedup.
- The shared abstraction adds generic-parameter complexity for a
  two-callsite payoff.

If you do this, do it after the `RefreshTrigger` factor-out has
shipped — they're independent concerns and bundling them slows review.

## Open questions

1. **Should `RefreshTrigger` live in `dbus_client.rs` or a new
   `tisvlay-core/src/watcher.rs`?** Current plan is to keep it in
   `dbus_client.rs` because it's coupled to `StateWatcher` in docs/usage.
   Alternative: split into `watcher.rs` and move `StateWatcher` along
   with it (bigger refactor, cleaner module boundaries).

2. **Naming.** Considered `RefreshLatch` (overloaded with std types),
   `Refresher` (verb-y), `Debouncer` (technically wrong — no time
   delay). `RefreshTrigger` reads cleanly at call sites:
   `trigger.fire(...)`. Open to alternatives.

3. **`Default` impl?** Yes — added above. Simplifies `RefreshTrigger::new()` to `RefreshTrigger::default()` if preferred.

## Files known to be modified by this refactor

- `crates/tisvlay-core/src/dbus_client.rs` (+ ~50 lines for `RefreshTrigger`)
- `crates/tisvlay-core/src/lib.rs` (+1 line: re-export)
- `crates/tisvlay-tray/src/main.rs` (− ~20 lines, simpler `watch_state`)
- `crates/tisvlay-gui/src/main.rs` (− ~25 lines, simpler `watch_state`)

Net: ~5 lines added, dedup of the contract footgun.
