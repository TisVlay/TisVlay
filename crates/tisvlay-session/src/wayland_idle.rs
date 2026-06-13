//! Session greeter-wake policy over the shared Wayland idle transport.
//!
//! Wraps `tisvlay_core`'s policy-free `ext-idle-notify-v1` watcher with the
//! session's lock-aware rule: while the screen is locked, the first input
//! after going idle emits a single `SessionEvent::GreeterWake` (so the
//! greeter can be woken to authenticate).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub use tisvlay_core::wayland::WaylandIdleError;
use tisvlay_core::wayland::{IdleTransition, start_idle_watch};

use crate::session_monitor::SessionEvent;

/// Idle timeout for greeter-wake detection, in milliseconds.
///
/// Must be short enough that the user goes "idle" in the brief pause between
/// locking the screen (e.g. Super+L) and moving the mouse to wake the
/// greeter: `ext-idle-notify-v1` only emits a `Resumed` after an `Idled`, so
/// at 200 ms even a rapid lock-then-move clears the threshold.
const GREETER_WAKE_TIMEOUT_MS: u32 = 200;

/// Start greeter-wake detection: a Wayland idle watch plus the lock-aware
/// policy task that turns a resume-while-locked into one `GreeterWake`.
///
/// Returns `Ok(())` once the watch thread is spawned, or an error if Wayland
/// is unavailable or the protocol is missing (not fatal — the caller logs and
/// continues, e.g. on X11/SSH).
pub fn start_wayland_idle_monitor(
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    is_locked: Arc<AtomicBool>,
) -> Result<(), WaylandIdleError> {
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<IdleTransition>();
    start_idle_watch(GREETER_WAKE_TIMEOUT_MS, raw_tx)?;

    tokio::spawn(async move {
        // Fire GreeterWake at most once per lock cycle; reset when unlocked.
        let mut greeter_wake_sent = false;
        while let Some(transition) = raw_rx.recv().await {
            if transition == IdleTransition::Resumed
                && is_locked.load(Ordering::SeqCst)
                && !greeter_wake_sent
            {
                debug!("User activity detected while locked - sending GreeterWake");
                greeter_wake_sent = true;
                if event_tx.send(SessionEvent::GreeterWake).is_err() {
                    warn!("Event channel closed");
                }
            }

            // Reset once the screen is unlocked (mirrors the prior
            // per-dispatch reset, so the next lock cycle can wake again).
            if !is_locked.load(Ordering::SeqCst) {
                greeter_wake_sent = false;
            }
        }
        warn!("Wayland idle transition channel closed");
    });

    Ok(())
}
