//! Brightness sync coordinator.
//!
//! Dispatches to exactly one backend based on the resolved user
//! profiles across all managed TVs. Variants are mutually exclusive
//! with the daemon's kernel-module path — see
//! `crates/tisvlay-core/src/config.rs::SyncBacklight`.
//!
//! # Dispatch rule
//!
//! - Any TV with `Kde` → [`kde`] (full orchestrator: Wayland + D-Bus,
//!   serves all KDE flavors).
//! - Else any TV with `KdeWayland` → [`kde_wayland`] (strict Wayland,
//!   forward only).
//! - Else any TV with `KdeDbus` → [`kde_dbus`] (strict D-Bus,
//!   bidirectional).
//! - Else (all `None` or `Kernel`) → wait until a profile change or
//!   daemon arrival repopulates `state.profiles`, then re-evaluate.

use std::sync::Arc;
use std::time::Duration;

use tisvlay_core::{DaemonClient, SyncBacklight};
use tokio::sync::{RwLock, watch};
use tracing::info;

use crate::SessionState;

pub mod kde;
pub mod kde_dbus;
pub mod kde_wayland;
pub mod shared;

/// Run the brightness sync coordinator until shutdown.
///
/// Session startup commonly races with daemon availability: if the
/// daemon isn't up yet, `state.profiles` is empty and we don't yet
/// know whether any TV wants a KDE-flavor backend. Poll state until
/// it's populated (or the user reconfigures mid-session), then
/// dispatch to exactly one backend.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let modes = match wait_for_modes(&state, &mut shutdown).await {
        Some(m) => m,
        None => return,
    };

    if modes.contains(&SyncBacklight::Kde) {
        kde::run(daemon, state, shutdown).await;
    } else if modes.contains(&SyncBacklight::KdeWayland) {
        kde_wayland::run(daemon, state, shutdown).await;
    } else if modes.contains(&SyncBacklight::KdeDbus) {
        kde_dbus::run(daemon, state, shutdown).await;
    } else {
        info!(
            ?modes,
            "brightness: no KDE-flavor TVs, session brightness sync idle"
        );
    }
}

/// Block until `state.profiles` has at least one entry (so we can see
/// the resolved modes) or shutdown is signalled. Returns the snapshot
/// of modes, or `None` if shutdown arrived first.
///
/// `SessionState::load` runs before this task starts but quietly
/// leaves `profiles` empty when the daemon is offline at boot. A
/// retry path in `main.rs` reloads state once the daemon appears —
/// the polling loop here catches that transition without the
/// complexity of a dedicated notifier channel.
async fn wait_for_modes(
    state: &Arc<RwLock<SessionState>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<Vec<SyncBacklight>> {
    loop {
        let modes = collect_modes(state).await;
        if !modes.is_empty() {
            return Some(modes);
        }
        tokio::select! {
            _ = shutdown.changed() => return None,
            _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
        }
    }
}

/// Snapshot of every TV's resolved `sync_backlight` value.
async fn collect_modes(state: &Arc<RwLock<SessionState>>) -> Vec<SyncBacklight> {
    state
        .read()
        .await
        .profiles
        .values()
        .map(|p| p.sync_backlight)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;
    use tisvlay_core::{DaemonClient, ResolvedUserProfile, UserConfig, UserProfileConfig};

    /// Minimal `SessionState` with no resolved profiles — the shape
    /// `SessionState::load` leaves behind when the daemon is offline
    /// at session startup.
    fn empty_state() -> Arc<RwLock<SessionState>> {
        Arc::new(RwLock::new(SessionState {
            profiles: HashMap::new(),
            user_config: UserConfig::default(),
            admin_user_default: UserProfileConfig::default(),
            admin_user_profiles: HashMap::new(),
            tv_ids: Vec::new(),
        }))
    }

    /// Regression: the dispatcher must not give up when `state.profiles`
    /// starts empty. The session commonly races with daemon availability
    /// at boot — the first `SessionState::load` call leaves `profiles`
    /// empty when the daemon is offline, and `main.rs` reloads state a
    /// few seconds later once it appears. Before the fix, the dispatcher
    /// would snapshot the empty map and exit permanently, leaving
    /// brightness sync silently disabled until the user restarted the
    /// session.
    ///
    /// Observed via `run()` directly so the test compiles against both
    /// the buggy and the fixed code — the buggy version exits
    /// immediately (task finishes at t=0) and the fixed version waits.
    #[tokio::test(start_paused = true)]
    async fn run_does_not_exit_while_state_is_empty() {
        let bus = tisvlay_test::PrivateBus::shared();
        let conn = bus.connect().await.expect("private bus connect");
        let daemon = Arc::new(
            DaemonClient::from_connection(conn)
                .await
                .expect("daemon client"),
        );

        let state = empty_state();
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(run(daemon, state.clone(), shutdown_rx));

        // Advance past what the old code would have needed to snapshot
        // `modes`, hit the `else info!(...)` branch, and return.
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;

        assert!(
            !task.is_finished(),
            "brightness::run must not exit while state.profiles is empty"
        );

        // Populate profiles — the way `main.rs` re-fills state when the
        // daemon finally appears. The dispatcher would then choose a
        // KDE backend; we don't care which (this test isolates the
        // polling fix), so cancel the task before it tries to register
        // a real Wayland device.
        {
            let mut s = state.write().await;
            s.profiles.insert(
                "office".to_string(),
                ResolvedUserProfile {
                    sync_backlight: SyncBacklight::Kde,
                    ..ResolvedUserProfile::default()
                },
            );
        }

        task.abort();
        let _ = task.await;
    }
}
