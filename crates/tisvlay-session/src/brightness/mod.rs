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
//! - Else (all `None` or `Kernel`) → stay in the wait loop until a
//!   KDE-flavor profile appears (either from a `SetTvUserProfile` flip
//!   at runtime or from the daemon populating `state.profiles` at
//!   boot), then dispatch.

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
/// a TV with a KDE-flavor profile appears (or the user flips one in
/// at runtime via `SetTvUserProfile`), then dispatch to exactly one
/// backend.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let modes = match wait_for_kde_modes(&state, &mut shutdown).await {
        Some(m) => m,
        None => return,
    };

    if modes.contains(&SyncBacklight::Kde) {
        kde::run(daemon, state, shutdown).await;
    } else if modes.contains(&SyncBacklight::KdeWayland) {
        kde_wayland::run(daemon, state, shutdown).await;
    } else if modes.contains(&SyncBacklight::KdeDbus) {
        kde_dbus::run(daemon, state, shutdown).await;
    }
}

/// Block until at least one TV's resolved profile is a KDE-flavor
/// mode (`Kde`, `KdeWayland`, or `KdeDbus`), or shutdown is signalled.
///
/// Two boot-time races motivate the polling loop:
///
/// - `SessionState::load` runs before this task starts but quietly
///   leaves `profiles` empty when the daemon is offline; a retry path
///   in `main.rs` reloads state once the daemon appears.
/// - The initial config may resolve to all-`Kernel` or all-`None`
///   TVs; a later `SetTvUserProfile` flip (from the tray or D-Bus)
///   mutates `state.profiles` in place, and the dispatcher has to
///   pick it up without a session restart.
///
/// Logs a one-shot `info!` when it transitions to the waiting state
/// so the idle case stays observable without flooding on every poll.
async fn wait_for_kde_modes(
    state: &Arc<RwLock<SessionState>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<Vec<SyncBacklight>> {
    let mut logged_waiting = false;
    loop {
        let modes = collect_modes(state).await;
        if modes.iter().any(is_kde_mode) {
            return Some(modes);
        }
        if !logged_waiting {
            info!(
                ?modes,
                "brightness: no KDE-flavor TV profiles yet, waiting for profile change"
            );
            logged_waiting = true;
        }
        tokio::select! {
            _ = shutdown.changed() => return None,
            _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
        }
    }
}

fn is_kde_mode(m: &SyncBacklight) -> bool {
    matches!(
        m,
        SyncBacklight::Kde | SyncBacklight::KdeWayland | SyncBacklight::KdeDbus
    )
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

    /// Regression: `brightness::run` used to exit immediately when
    /// `state.profiles` was non-empty but contained no KDE-flavor
    /// modes. For all-`Kernel` or all-`None` deployments, the
    /// dispatcher would log `no KDE-flavor TVs, session brightness
    /// sync idle` and return — a later `SetTvUserProfile` flip to
    /// `Kde`/`KdeWayland`/`KdeDbus` could not revive it, because the
    /// dispatcher task was already finished. The common trigger is a
    /// user reassigning a TV's profile at runtime (e.g. from the tray
    /// or via `busctl call ... SetTvUserProfile`).
    ///
    /// The fix gates `wait_for_modes` on the presence of at least one
    /// KDE-flavor mode; it keeps polling at 1Hz until one appears, so
    /// runtime profile flips take effect without a session restart.
    #[tokio::test(start_paused = true)]
    async fn run_keeps_polling_when_only_non_kde_profiles_present() {
        let bus = tisvlay_test::PrivateBus::shared();
        let conn = bus.connect().await.expect("private bus connect");
        let daemon = Arc::new(
            DaemonClient::from_connection(conn)
                .await
                .expect("daemon client"),
        );

        let state = empty_state();
        {
            let mut s = state.write().await;
            s.profiles.insert(
                "office".to_string(),
                ResolvedUserProfile {
                    sync_backlight: SyncBacklight::Kernel,
                    ..ResolvedUserProfile::default()
                },
            );
        }

        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run(daemon, state.clone(), shutdown_rx));

        // Advance past what the buggy code would need to hit the
        // `idle` branch and return.
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;

        assert!(
            !task.is_finished(),
            "brightness::run must keep polling for a KDE-flavor profile; \
             exiting early means runtime SetTvUserProfile flips (Kernel \u{2192} Kde) \
             never take effect without a session restart"
        );

        task.abort();
        let _ = task.await;
    }
}
