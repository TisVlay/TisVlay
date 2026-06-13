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
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;
use tisvlay_core::DaemonClient;
use tisvlay_core::wayland::{IdleNotification, IdleTransition, start_idle_watch_on_socket};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

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
/// cleanly. On startup it sends one `NotifyGreeterIdle(false)` to assert the
/// login screen is present (lit), so a screen-off latched by the prior user
/// session doesn't strand it dark. It deliberately does *not* send a resume on
/// exit — the user session that follows owns re-enabling the screen.
pub async fn run(daemon: DaemonClient) -> Result<()> {
    // Read once at greeter-session start. A config change made while a greeter
    // is live applies on the next greeter session (each is a fresh process);
    // the greeter is short-lived, so this is acceptable.
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

    let (tx, mut rx) = mpsc::unbounded_channel::<(u32, IdleTransition)>();
    connect_idle_watch(timeout_ms, &tx).await?;
    // The watch thread holds its own sender clone; drop ours so `rx` closes
    // when the compositor disconnects (greeter session ends).
    drop(tx);

    // Assert the greeter is present (panel on) at startup, before the first
    // idle edge. A prior user session may have latched the panel off (e.g.
    // logout from a locked session); `NotifyGreeterIdle(false)` maps to the
    // resume action so the login screen is lit for the person now at it. This
    // also primes the de-dup so the first real `Idled` edge still fires.
    let mut last_sent: Option<bool> = None;
    if notify_with_retry(&daemon, false).await {
        last_sent = Some(false);
    }

    while let Some((_tag, transition)) = rx.recv().await {
        let Some(idle) = next_notify(transition, last_sent) else {
            continue;
        };
        if notify_with_retry(&daemon, idle).await {
            last_sent = Some(idle);
        }
    }

    // Reached when the watch thread exits (the compositor socket closed). The
    // common cause is the greeter session ending at login, where systemd is
    // already stopping this unit — returning Ok is correct and avoids logging
    // the normal teardown as an error. The disabled (timeout 0) path above
    // also returns Ok; the unit's Restart=on-failure deliberately does NOT
    // restart these clean exits (Restart=always would busy-loop the disabled
    // case). A fresh greeter session re-creates the unit.
    warn!("Greeter idle watch ended (compositor disconnected); exiting");
    Ok(())
}

/// Send a greeter idle/resume notification, retrying briefly so a transient
/// daemon hiccup (e.g. a restart) doesn't strand the screen state — at an
/// unattended login screen there may be no further input edge to retry on.
/// Returns whether it eventually succeeded.
async fn notify_with_retry(daemon: &DaemonClient, idle: bool) -> bool {
    const MAX_ATTEMPTS: u32 = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        match daemon.notify_greeter_idle(idle).await {
            Ok(()) => return true,
            Err(e) if attempt == MAX_ATTEMPTS => {
                warn!(error = %e, idle, "NotifyGreeterIdle failed after retries");
                return false;
            }
            Err(e) => {
                debug!(attempt, error = %e, idle, "NotifyGreeterIdle failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    false
}

/// Discover the greeter compositor's wayland socket under `XDG_RUNTIME_DIR`
/// and start the idle watch, retrying while the socket appears.
///
/// The greeter `systemd.user` unit carries no `WAYLAND_DISPLAY`, and
/// wayland-client's `connect_to_env` *requires* it — with it unset, `connect_to_env`
/// returns `NoCompositor` rather than falling back to `wayland-0` the way
/// libwayland's C client does. So we discover the actual `wayland-<n>` socket
/// under `XDG_RUNTIME_DIR` and connect to it explicitly. The retry covers the
/// socket landing a moment after our unit (ordered after kwin's bus name) starts.
async fn connect_idle_watch(
    timeout_ms: u32,
    tx: &mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<()> {
    const MAX_ATTEMPTS: u32 = 15;
    let mut last_err = anyhow!("no connection attempt made");
    for attempt in 1..=MAX_ATTEMPTS {
        match try_connect(timeout_ms, tx.clone()) {
            Ok(()) => return Ok(()),
            Err(e) => {
                debug!(attempt, error = %e, "greeter compositor not ready; retrying");
                last_err = e;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    warn!(
        xdg_runtime_dir = ?std::env::var("XDG_RUNTIME_DIR").ok(),
        wayland_display = ?std::env::var("WAYLAND_DISPLAY").ok(),
        sockets = ?wayland_sockets(),
        "Could not connect to the greeter compositor after retries"
    );
    Err(last_err.context("connect to greeter compositor"))
}

fn try_connect(timeout_ms: u32, tx: mpsc::UnboundedSender<(u32, IdleTransition)>) -> Result<()> {
    let path =
        greeter_socket_path().ok_or_else(|| anyhow!("no wayland socket under XDG_RUNTIME_DIR"))?;
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connect wayland socket {}", path.display()))?;
    // A single notification (the greeter idle timeout); tag is unused here.
    start_idle_watch_on_socket(stream, vec![IdleNotification { tag: 0, timeout_ms }], tx)
        .map_err(|e| anyhow!("{e}"))
}

/// Resolve the compositor socket path: `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` when
/// set, else the first `wayland-<n>` socket in `$XDG_RUNTIME_DIR`.
fn greeter_socket_path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if let Some(disp) = std::env::var_os("WAYLAND_DISPLAY") {
        let disp = PathBuf::from(disp);
        return Some(if disp.is_absolute() {
            disp
        } else {
            dir.join(disp)
        });
    }
    wayland_sockets()
        .into_iter()
        .next()
        .map(|name| dir.join(name))
}

/// Sorted `wayland-<n>` socket names present in `$XDG_RUNTIME_DIR` (the `.lock`
/// companions are excluded). Also used for failure diagnostics.
fn wayland_sockets() -> Vec<String> {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.strip_prefix("wayland-")
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    names.sort();
    names
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
