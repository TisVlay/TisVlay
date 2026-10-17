//! Spawn-with-dedupe trigger for refresh-style work.
//!
//! See [`RefreshTrigger`] for the full pattern and the bug history
//! that motivated extracting it.

/// Spawn-with-dedupe trigger for refresh-style work driven by
/// state-watcher events.
///
/// The canonical pattern for consuming a state watcher is to dispatch
/// each `DaemonStateChanged` (and friends) to `RefreshTrigger::fire()`,
/// which spawns the refresh on a separate task. The watcher loop body
/// MUST NOT `await` the refresh inline — see "Why this matters" below.
///
/// At most one refresh is in flight. Fires that arrive while a refresh
/// is running set a `dirty` flag; when the in-flight refresh completes
/// the worker checks the flag and runs another refresh without
/// releasing the in-flight slot. This guarantees trailing-edge
/// correctness — a flood that ends exactly during a refresh still
/// captures the post-flood state.
///
/// # Example
///
/// ```ignore
/// let trigger = RefreshTrigger::new();
/// loop {
///     let Some(event) = watcher.next().await else { break };
///     match event {
///         StateEvent::DaemonStateChanged(_)
///         | StateEvent::DaemonAppeared
///         | StateEvent::TvUserProfileChanged
///         | StateEvent::FallbackPoll => {
///             let daemon = daemon.clone();
///             let session = session.clone();
///             let handle = handle.clone();
///             trigger.fire(move || {
///                 let daemon = daemon.clone();
///                 let session = session.clone();
///                 let handle = handle.clone();
///                 async move { refresh_state(&daemon, &session, &handle).await; }
///             });
///         }
///         /* … pane-specific dispatch for the other events … */
///     }
/// }
/// ```
///
/// # Why this matters
///
/// Awaiting refresh work inline in the `watcher.next()` loop body
/// halts event-stream draining → the transport's bounded internal
/// channel fills → its I/O task blocks pushing → the connection stops
/// reading from the socket → replies queue behind unread event frames
/// in the kernel recv buffer. Subsequent client commands hang waiting
/// for replies that can't be read.
///
/// Verified repro for the bug fixed by introducing this helper
/// (observed on the root project's D-Bus stack; the failure mode is
/// transport-agnostic): a fast volume-slider drag in the GUI flooded
/// the daemon, the tray's recv-q grew to 36 KB, and a subsequent
/// ScrollBy command hung indefinitely while an out-of-band probe of
/// the same daemon replied in 4 ms — confirming the daemon was
/// healthy and the tray's connection was wedged on its own
/// consumption pattern.
pub struct RefreshTrigger {
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for RefreshTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::sync::atomic::Ordering;
        f.debug_struct("RefreshTrigger")
            .field("in_flight", &self.in_flight.load(Ordering::Relaxed))
            .field("dirty", &self.dirty.load(Ordering::Relaxed))
            .finish()
    }
}

impl RefreshTrigger {
    /// An idle trigger: no refresh in flight, nothing pending.
    pub fn new() -> Self {
        Self {
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark a refresh as needed; spawn a worker task if one isn't
    /// already running. The worker loops as long as `dirty` is set,
    /// guaranteeing a final refresh after any flood that ended during
    /// an earlier iteration.
    ///
    /// `refresh_fn` may be invoked multiple times across iterations,
    /// so it's `Fn` (not `FnOnce`) and must be `Sync` because the
    /// worker runs on a tokio task. Closures that capture `Arc<T>`
    /// and clone inside their body satisfy this; closures that
    /// consume captured values do not, which is the desired
    /// behavior — those would need re-creation per fire.
    pub fn fire<F, Fut>(&self, refresh_fn: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send,
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use parking_lot::Mutex;
    use tokio::sync::{mpsc, oneshot};
    use tokio::time::timeout;

    use super::RefreshTrigger;

    #[tokio::test(flavor = "multi_thread")]
    async fn fires_during_refresh_schedule_one_trailing_refresh() {
        let trigger = RefreshTrigger::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Mutex::new(None::<oneshot::Sender<()>>));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (completed_tx, mut completed_rx) = mpsc::unbounded_channel();

        let refresh = {
            let runs = Arc::clone(&runs);
            let release = Arc::clone(&release);
            Arc::new(move || -> Pin<Box<dyn Future<Output = ()> + Send>> {
                let run = runs.fetch_add(1, Ordering::SeqCst);
                let release = Arc::clone(&release);
                let started_tx = started_tx.clone();
                let completed_tx = completed_tx.clone();
                Box::pin(async move {
                    if run == 0 {
                        let (tx, rx) = oneshot::channel();
                        *release.lock() = Some(tx);
                        started_tx.send(()).expect("test awaits first refresh");
                        rx.await.expect("test releases first refresh");
                    }
                    completed_tx
                        .send(run)
                        .expect("test awaits refresh completion");
                })
            })
                as Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>
        };

        let first = Arc::clone(&refresh);
        trigger.fire(move || first());
        timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .expect("first refresh starts")
            .expect("first refresh signals its start");

        for _ in 0..3 {
            let refresh = Arc::clone(&refresh);
            trigger.fire(move || refresh());
        }
        release
            .lock()
            .take()
            .expect("first refresh stores its release")
            .send(())
            .expect("first refresh is waiting");

        assert_eq!(
            timeout(Duration::from_secs(1), completed_rx.recv())
                .await
                .expect("first refresh completes")
                .expect("first refresh sends its run index"),
            0
        );
        assert_eq!(
            timeout(Duration::from_secs(1), completed_rx.recv())
                .await
                .expect("trailing refresh completes")
                .expect("trailing refresh sends its run index"),
            1
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
