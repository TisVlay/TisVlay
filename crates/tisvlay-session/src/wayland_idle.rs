//! Session idle/lock-wake policy over the shared Wayland idle transport.
//!
//! Wraps `tisvlay_core`'s policy-free `ext-idle-notify-v1` watcher with the
//! session's two presence rules, on one connection via tagged notifications:
//! - **Lock-wake** (short timeout): while the screen is locked, the first
//!   input after going idle emits a single `SessionEvent::LockWake`, so the
//!   panel comes back to show the password prompt.
//! - **In-session idle** (the user profile's `idle_timeout_seconds`): an
//!   `Idled`/`Resumed` edge emits `SessionEvent::IdleTimeout`/`IdleResume`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

pub use tisvlay_core::wayland::WaylandIdleError;
use tisvlay_core::wayland::{IdleNotification, IdleTransition, WatchHandle, start_idle_watch};

use crate::session_monitor::SessionEvent;

/// Idle timeout for lock-wake detection, in milliseconds.
///
/// Must be short enough that the user goes "idle" in the brief pause between
/// locking the screen (e.g. Super+L) and moving the mouse to wake the
/// greeter: `ext-idle-notify-v1` only emits a `Resumed` after an `Idled`, so
/// at 200 ms even a rapid lock-then-move clears the threshold.
const LOCK_WAKE_TIMEOUT_MS: u32 = 200;

/// Notification tag for the lock-wake watch.
const LOCK_WAKE_TAG: u32 = 0;

/// Notification tag for the in-session idle watch (registered only when the
/// resolved `idle_timeout_seconds` is non-zero).
const IDLE_TAG: u32 = 1;

/// Start the session's Wayland presence watches on one connection: a short
/// lock-wake notification (always) and, when `idle_timeout_ms` is `Some`, an
/// in-session idle notification. Transitions are routed by tag to the
/// lock-aware `LockWake` policy or to `IdleTimeout`/`IdleResume`.
///
/// Returns the [`WatchHandle`] (hold it to keep the watch running; dropping it
/// stops the watch and ends the policy task) plus a [`oneshot::Receiver`] that
/// fires when the watch thread dies — the compositor disconnected — so the
/// supervisor can restart it. Errors if Wayland is unavailable or the protocol
/// is missing (not fatal — the caller logs and continues, e.g. on X11/SSH).
pub fn start_wayland_idle_monitor(
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    is_locked: Arc<AtomicBool>,
    idle_timeout_ms: Option<u32>,
) -> Result<(WatchHandle, oneshot::Receiver<()>), WaylandIdleError> {
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<(u32, IdleTransition)>();
    let mut notifications = vec![IdleNotification {
        tag: LOCK_WAKE_TAG,
        timeout_ms: LOCK_WAKE_TIMEOUT_MS,
    }];
    if let Some(timeout_ms) = idle_timeout_ms {
        notifications.push(IdleNotification {
            tag: IDLE_TAG,
            timeout_ms,
        });
    }
    let handle = start_idle_watch(notifications, raw_tx)?;

    // Fires when the watch thread dies (compositor disconnected, closing
    // `raw_rx`), so the supervisor can restart the watch.
    let (dead_tx, dead_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut lock_wake_sent = false;
        while let Some((tag, transition)) = raw_rx.recv().await {
            match tag {
                LOCK_WAKE_TAG => {
                    let (next_sent, fire) = lock_wake_policy(
                        transition,
                        is_locked.load(Ordering::SeqCst),
                        lock_wake_sent,
                    );
                    lock_wake_sent = next_sent;
                    if fire {
                        debug!("User activity detected while locked - sending LockWake");
                        if event_tx.send(SessionEvent::LockWake).is_err() {
                            warn!("Event channel closed");
                        }
                    }
                }
                IDLE_TAG => {
                    let event = match transition {
                        IdleTransition::Idled => SessionEvent::IdleTimeout,
                        IdleTransition::Resumed => SessionEvent::IdleResume,
                    };
                    debug!(event = %event, "In-session idle transition");
                    if event_tx.send(event).is_err() {
                        warn!("Event channel closed");
                    }
                }
                _ => {}
            }
        }
        warn!("Wayland idle transition channel closed");
        // Tell the supervisor the watch ended; a dropped receiver (it already
        // re-registered) is fine.
        let _ = dead_tx.send(());
    });

    Ok((handle, dead_rx))
}

/// Lock-screen wake decision for one idle transition.
///
/// Returns `(next_lock_wake_sent, fire)`. Fires `LockWake` on the first
/// `Resumed` (user activity) of each idle cycle while the screen is locked, at
/// most once per cycle. The next `Idled` clears the latch (a fresh idle cycle
/// begins), so a panel that re-blanks within one lock session wakes again, and
/// a rapid unlock→relock that produced no "unlocked" edge still wakes on the
/// next idle cycle. While unlocked the latch is never set, so no wake fires.
/// Because the latch is per idle cycle (not per lock session), a watch
/// re-registration mid-session can't strand or duplicate a wake.
fn lock_wake_policy(
    transition: IdleTransition,
    is_locked: bool,
    lock_wake_sent: bool,
) -> (bool, bool) {
    match transition {
        // A fresh idle cycle clears the latch so the next resume can wake,
        // independent of any "unlocked" edge — robust to a panel that re-blanks
        // within one lock session and to a rapid unlock→relock. Idle itself
        // never fires a wake.
        IdleTransition::Idled => (false, false),
        IdleTransition::Resumed => {
            let fire = is_locked && !lock_wake_sent;
            // Stay latched (meaningful only while locked) until the next Idled,
            // so a burst of resume events doesn't spam LockWake.
            let next_sent = is_locked && (lock_wake_sent || fire);
            (next_sent, fire)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_per_idle_cycle_on_resume() {
        // Idled while locked: no fire, latch cleared (new idle cycle).
        assert_eq!(
            lock_wake_policy(IdleTransition::Idled, true, false),
            (false, false)
        );
        // First Resumed while locked: fire and latch.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, false),
            (true, true)
        );
        // Second Resumed while still locked (already latched): no fire.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, true),
            (true, false)
        );
    }

    #[test]
    fn idled_clears_latch_so_each_idle_cycle_can_wake() {
        // First resume of a locked idle cycle fires and latches.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, false),
            (true, true)
        );
        // A fresh Idled while still locked clears the latch — even across a
        // rapid unlock/relock that produced no "unlocked" edge.
        assert_eq!(
            lock_wake_policy(IdleTransition::Idled, true, true),
            (false, false)
        );
        // So the next resume fires again.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, false),
            (true, true)
        );
    }

    #[test]
    fn resets_when_unlocked_and_never_fires() {
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, false, true),
            (false, false)
        );
        assert_eq!(
            lock_wake_policy(IdleTransition::Idled, false, true),
            (false, false)
        );
    }
}
