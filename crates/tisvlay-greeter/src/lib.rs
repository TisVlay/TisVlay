//! tisvlay-greeter — login-screen idle client.
//!
//! Runs inside the greeter session (as the greeter user) and subscribes to
//! the greeter compositor's `ext-idle-notify-v1`. On idle/resume it calls the
//! system daemon's `Manager1.NotifyGreeterIdle`, which blanks/restores managed
//! TVs per their system-profile `on_idle_timeout` / `on_idle_resume`. This is
//! the only component running before login, so it's what protects an OLED
//! panel at an unattended login screen.
//!
//! The timeout to register comes from the daemon
//! (`Manager1.GreeterIdleTimeoutSeconds`): `0` means "disabled", and the
//! process exits without watching.

use anyhow::{Context, Result, anyhow};
use tisvlay_core::DaemonClient;
use tisvlay_core::wayland::{IdleTransition, start_idle_watch};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Decide what (if anything) to send for a transition, given the last idle
/// state we successfully reported. Returns `Some(idle)` to call
/// `NotifyGreeterIdle(idle)`, or `None` to skip a redundant repeat (the
/// daemon's dispatch is idempotent, but skipping keeps the bus quiet and
/// matches the session path's de-dup stance).
fn next_notify(transition: IdleTransition, last_sent: Option<bool>) -> Option<bool> {
    let idle = matches!(transition, IdleTransition::Idled);
    if last_sent == Some(idle) {
        None
    } else {
        Some(idle)
    }
}

/// Connect the greeter compositor's idle notifications to the daemon.
///
/// Queries the daemon for the greeter idle timeout; if `0`, returns
/// immediately (feature disabled). Otherwise registers an
/// `ext-idle-notify-v1` watch and forwards each idle/resume edge to
/// `Manager1.NotifyGreeterIdle` until the compositor disconnects (greeter
/// session ended), at which point it returns `Ok(())` so the process exits
/// cleanly. It deliberately does *not* send a resume on exit — the user
/// session that follows owns re-enabling the screen.
pub async fn run(daemon: DaemonClient) -> Result<()> {
    let timeout_secs = daemon
        .greeter_idle_timeout_seconds()
        .await
        .context("query greeter idle timeout")?;

    if timeout_secs == 0 {
        info!("Greeter idle timeout is 0 (disabled); nothing to watch");
        return Ok(());
    }

    let timeout_ms = timeout_secs.saturating_mul(1000).min(u32::MAX as u64) as u32;
    info!(timeout_secs, "Starting greeter idle watch");

    let (tx, mut rx) = mpsc::unbounded_channel::<IdleTransition>();
    start_idle_watch(timeout_ms, tx).map_err(|e| anyhow!("start greeter idle watch: {e}"))?;

    let mut last_sent: Option<bool> = None;
    while let Some(transition) = rx.recv().await {
        let Some(idle) = next_notify(transition, last_sent) else {
            continue;
        };
        match daemon.notify_greeter_idle(idle).await {
            Ok(()) => last_sent = Some(idle),
            Err(e) => warn!(error = %e, idle, "NotifyGreeterIdle failed; retrying on next edge"),
        }
    }

    warn!("Greeter idle watch ended (compositor disconnected); exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_transition_always_sends() {
        assert_eq!(next_notify(IdleTransition::Idled, None), Some(true));
        assert_eq!(next_notify(IdleTransition::Resumed, None), Some(false));
    }

    #[test]
    fn repeated_same_state_is_skipped() {
        assert_eq!(next_notify(IdleTransition::Idled, Some(true)), None);
        assert_eq!(next_notify(IdleTransition::Resumed, Some(false)), None);
    }

    #[test]
    fn opposite_state_sends() {
        assert_eq!(
            next_notify(IdleTransition::Resumed, Some(true)),
            Some(false)
        );
        assert_eq!(next_notify(IdleTransition::Idled, Some(false)), Some(true));
    }
}
