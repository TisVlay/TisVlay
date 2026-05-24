//! Trailing-edge debouncer for `config_reloaded` notifications.
//!
//! Multi-row reloads (e.g. a hand-edit that adds two profiles and
//! reassigns one TV) arrive as a burst of `StateEvent::{System,User,
//! TvUser}ProfileChanged` / …Added / …Removed events. Without
//! debouncing, the user would see a clatter of notifications for one
//! conceptual reload. [`Debouncer`] coalesces a burst into a single
//! trailing-edge call to the configured emit closure: every `bump()`
//! restarts the window; the closure fires once after the window
//! elapses with no further bumps.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

#[derive(Clone)]
pub(crate) struct Debouncer {
    state: Arc<DebouncerState>,
}

struct DebouncerState {
    /// Notify-on-bump primitive. Each `bump()` wakes the worker
    /// task, which resets its sleep timer to `window` and waits for
    /// either the next bump or the timer to expire.
    notify: Notify,
    /// Window length — the worker sleeps for this long after every
    /// bump. Trailing-edge means the emit fires exactly once at
    /// `last_bump + window`, regardless of how many bumps came in
    /// during the burst.
    window: Duration,
    /// Inner counter — incremented per bump so the worker can
    /// detect "was bumped during my sleep" by comparing pre- and
    /// post-sleep counter values. `std::Mutex` because the critical
    /// section is one integer load/store with no await.
    counter: Mutex<u64>,
}

impl Debouncer {
    pub(crate) fn new<F>(window: Duration, on_fire: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let state = Arc::new(DebouncerState {
            notify: Notify::new(),
            window,
            counter: Mutex::new(0),
        });
        // Worker task: idle on `notify.notified()`; once woken,
        // sleep for `window` and re-check whether more bumps
        // arrived. Loop until the counter is stable across a full
        // window, then fire.
        //
        // Drain-permit gotcha: every `bump()` calls `notify_one()`,
        // which buffers one permit on `Notify`. Bumps that arrive
        // DURING the inner sleep+check loop extend the window via
        // the counter compare, but ALSO leave a buffered permit on
        // `Notify`. Without draining, the outer `notified().await`
        // returns immediately on the next iteration, causing a
        // spurious second fire one `window` later for bumps already
        // handled in this fire cycle. Manifests in practice as a
        // duplicate notification ~500ms after the legitimate one
        // (visible in journal even when Plasma's
        // `ExcessNotificationGeneration` rate-limit swallows it).
        // Fix: after firing, snapshot the counter and try to drain
        // any pending permit only if no new bumps have arrived in
        // the meantime.
        let worker_state = state.clone();
        tokio::spawn(async move {
            loop {
                worker_state.notify.notified().await;
                let fired_at;
                loop {
                    let before = *worker_state.counter.lock().expect("mutex poisoned");
                    tokio::time::sleep(worker_state.window).await;
                    let after = *worker_state.counter.lock().expect("mutex poisoned");
                    if after == before {
                        fired_at = after;
                        break;
                    }
                }
                on_fire();
                // Drain any buffered permit from bumps already
                // accounted for in this fire cycle. Skip the drain
                // if the counter has advanced since `fired_at` —
                // a genuinely-new bump (arrived after we decided to
                // fire) deserves its own future fire.
                if *worker_state.counter.lock().expect("mutex poisoned") == fired_at {
                    let mut drain = std::pin::pin!(worker_state.notify.notified());
                    drain.as_mut().enable();
                    // `drain` dropped at end of scope; any pending
                    // permit was consumed by `enable()`.
                }
            }
        });
        Self { state }
    }

    pub(crate) fn bump(&self) {
        {
            let mut counter = self.state.counter.lock().expect("mutex poisoned");
            *counter = counter.wrapping_add(1);
        }
        self.state.notify.notify_one();
    }
}
