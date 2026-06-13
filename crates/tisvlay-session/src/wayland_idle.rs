//! Session lock-wake policy over the shared Wayland idle transport.
//!
//! Wraps `tisvlay_core`'s policy-free `ext-idle-notify-v1` watcher with the
//! session's lock-aware rule: while the screen is locked, the first input
//! after going idle emits a single `SessionEvent::LockWake` (so the
//! greeter can be woken to authenticate).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub use tisvlay_core::wayland::WaylandIdleError;
use tisvlay_core::wayland::{IdleNotification, IdleTransition, start_idle_watch};

use crate::session_monitor::SessionEvent;

/// Idle timeout for lock-wake detection, in milliseconds.
///
/// Must be short enough that the user goes "idle" in the brief pause between
/// locking the screen (e.g. Super+L) and moving the mouse to wake the
/// greeter: `ext-idle-notify-v1` only emits a `Resumed` after an `Idled`, so
/// at 200 ms even a rapid lock-then-move clears the threshold.
const LOCK_WAKE_TIMEOUT_MS: u32 = 200;

/// Notification tag for the lock-wake watch (the only one today; the in-session
/// idle watch will register a second tag on the same connection).
const LOCK_WAKE_TAG: u32 = 0;

/// Start lock-wake detection: a Wayland idle watch plus the lock-aware
/// policy task that turns a resume-while-locked into one `LockWake`.
///
/// Returns `Ok(())` once the watch thread is spawned, or an error if Wayland
/// is unavailable or the protocol is missing (not fatal — the caller logs and
/// continues, e.g. on X11/SSH).
pub fn start_wayland_idle_monitor(
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    is_locked: Arc<AtomicBool>,
) -> Result<(), WaylandIdleError> {
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<(u32, IdleTransition)>();
    start_idle_watch(
        vec![IdleNotification {
            tag: LOCK_WAKE_TAG,
            timeout_ms: LOCK_WAKE_TIMEOUT_MS,
        }],
        raw_tx,
    )?;

    tokio::spawn(async move {
        let mut lock_wake_sent = false;
        while let Some((_tag, transition)) = raw_rx.recv().await {
            let (next_sent, fire) =
                lock_wake_policy(transition, is_locked.load(Ordering::SeqCst), lock_wake_sent);
            lock_wake_sent = next_sent;
            if fire {
                debug!("User activity detected while locked - sending LockWake");
                if event_tx.send(SessionEvent::LockWake).is_err() {
                    warn!("Event channel closed");
                }
            }
        }
        warn!("Wayland idle transition channel closed");
    });

    Ok(())
}

/// Lock-screen wake decision for one idle transition.
///
/// Returns `(next_lock_wake_sent, fire)`. Fires `LockWake` on the first
/// `Resumed` (user activity) seen while the screen is locked, at most once per
/// lock cycle; the latch is cleared whenever the screen is observed unlocked,
/// so the next lock cycle can wake again. The transport forwards only
/// idle/resume edges, so the reset is observed on the next such edge rather
/// than on every wayland event as before — sufficient in practice, since any
/// pause longer than the 200 ms threshold produces one.
fn lock_wake_policy(
    transition: IdleTransition,
    is_locked: bool,
    lock_wake_sent: bool,
) -> (bool, bool) {
    let fire = transition == IdleTransition::Resumed && is_locked && !lock_wake_sent;
    let next_sent = if is_locked {
        lock_wake_sent || fire
    } else {
        false
    };
    (next_sent, fire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_per_lock_cycle_on_resume() {
        // Idled while locked: no fire, latch unchanged.
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
