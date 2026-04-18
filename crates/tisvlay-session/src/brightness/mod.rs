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
//! - Else (all `None` or `Kernel`) → return immediately; the daemon
//!   kernel-module path owns brightness for those TVs.

use std::sync::Arc;

use tisvlay_core::{DaemonClient, SyncBacklight};
use tokio::sync::{RwLock, watch};
use tracing::info;

use crate::SessionState;

pub mod kde;
pub mod kde_dbus;
pub mod kde_wayland;
pub mod shared;

/// Run the brightness sync coordinator until shutdown. Picks one
/// backend based on the TVs' resolved `sync_backlight`; exits
/// immediately if none need the session path.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    shutdown: watch::Receiver<bool>,
) {
    let modes = collect_modes(&state).await;

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
