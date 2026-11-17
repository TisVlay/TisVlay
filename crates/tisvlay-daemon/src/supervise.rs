//! The daemon's supervision loops, shared by its watchers.
//!
//! The kernel watchers (netlink routes, udev devices) reconnect through a
//! capped backoff and keep serving the world's requests while their source
//! is down: [`supervise`]. The bus watchers (logind, the seat watch)
//! reconnect at a fixed delay and warn once per outage: [`LossLatch`].

use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;

/// The first reconnect delay after a kernel watcher's source fails; each
/// failure doubles it up to [`RECONNECT_MAX`].
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// A watch that ran this long was working: its loss starts the backoff
/// over and warns again, so one that opens and dies at once backs off
/// instead of retrying every second, and a fault months into uptime isn't
/// hidden behind a warning spent at boot.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// A kernel watcher [`supervise`] keeps running for the world's lifetime.
/// It owns its view of the world, which survives reconnects.
pub(crate) trait Watch {
    type Request;

    /// One connection's lifetime: `Ok` once the world is gone, `Err` when
    /// the source fails and should be reopened.
    fn watch(
        &mut self,
        requests: &mut mpsc::UnboundedReceiver<Self::Request>,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Serve one request while the source is down; false once the world
    /// is gone.
    fn serve(&mut self, request: Self::Request) -> impl Future<Output = bool> + Send;

    /// Report the source lost, retrying after `retry_in`. The watcher logs
    /// it itself, so the warning keeps its module's target.
    fn lost(&self, error: &anyhow::Error, retry_in: Duration);
}

/// Run `watcher` for the world's lifetime: when its source fails, keep
/// serving requests through a capped backoff, then reconnect. Without
/// this a dead connection would silently end the watch, and a TV parked
/// on it would never redial.
pub(crate) async fn supervise<W: Watch>(
    mut watcher: W,
    mut requests: mpsc::UnboundedReceiver<W::Request>,
) {
    let mut backoff = RECONNECT_MIN;
    loop {
        let started = tokio::time::Instant::now();
        let error = match watcher.watch(&mut requests).await {
            Ok(()) => return,
            Err(error) => error,
        };
        if started.elapsed() >= HEALTHY_RUN {
            backoff = RECONNECT_MIN;
        }
        watcher.lost(&error, backoff);
        let pause = tokio::time::sleep(backoff);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(request) => {
                        if !watcher.serve(request).await {
                            return;
                        }
                    }
                    None => return,
                },
                () = &mut pause => break,
            }
        }
        backoff = backoff.saturating_mul(2).min(RECONNECT_MAX);
    }
}

/// Warn once per outage for a bus watch that reconnects at a fixed delay:
/// a bus that stays down doesn't repeat the warning, but a watch that ran
/// for [`HEALTHY_RUN`] was working, so its loss warns again.
#[derive(Debug, Default)]
pub(crate) struct LossLatch {
    warned: bool,
}

impl LossLatch {
    /// Whether the loss of a watch that ran for `ran` should warn.
    pub(crate) fn should_warn(&mut self, ran: Duration) -> bool {
        if ran >= HEALTHY_RUN {
            self.warned = false;
        }
        !std::mem::replace(&mut self.warned, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latch_warns_once_per_outage() {
        let mut latch = LossLatch::default();
        let quick = Duration::from_secs(1);
        assert!(latch.should_warn(quick), "the first loss warns");
        assert!(!latch.should_warn(quick), "a bus that stays down is quiet");
        assert!(
            latch.should_warn(HEALTHY_RUN),
            "a watch that ran a while was working; its loss warns again"
        );
        assert!(!latch.should_warn(quick));
    }
}
