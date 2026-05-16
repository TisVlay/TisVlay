//! tisvlay-session library — user session service logic.
//!
//! The binary entry point lives in `main.rs`; this library exposes the
//! modules that integration tests need to drive.

pub mod idle;
pub mod session_dbus;
pub mod session_monitor;
pub mod wayland_idle;

/// Re-exports for the integration-test harness crate. Not public API.
#[doc(hidden)]
pub mod __internals {
    pub use crate::SessionState;
    pub use crate::event_handler;
    pub use crate::reconcile_user_config_orphans;
    pub use crate::session_dbus::register_session_dbus_on;
    pub use crate::session_monitor::SessionEvent;
}

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{
    DaemonClient, QosConfig, ResolvedEventAction, ResolvedUserProfile, SessionAction, UserConfig,
    UserProfileConfig,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, warn};

/// Session state: resolved profiles per TV, kept in sync with config reloads.
#[derive(Debug)]
pub struct SessionState {
    /// Resolved user profiles per TV ID.
    pub profiles: HashMap<String, ResolvedUserProfile>,
    /// User config (used by Session D-Bus interface for profile management).
    pub user_config: UserConfig,
    /// Admin baseline for user profiles (from the daemon).
    pub admin_user_default: UserProfileConfig,
    /// All admin user profiles from the daemon (for listing available profiles).
    pub admin_user_profiles: HashMap<String, UserProfileConfig>,
    /// TV IDs from the daemon (ordered).
    pub tv_ids: Vec<String>,
}

impl SessionState {
    /// Build session state by querying daemon and loading user config.
    pub async fn load(daemon: &DaemonClient, user_config_path: Option<&PathBuf>) -> Result<Self> {
        // Query daemon for TV list
        let tv_infos = daemon
            .list_tvs()
            .await
            .context("Failed to get TV list from daemon")?;
        let tv_ids: Vec<String> = tv_infos.iter().map(|t| t.uuid.clone()).collect();

        // Query daemon for admin baseline user profile
        let admin_user_default = match daemon.get_user_profile_default().await {
            Ok(profile) => {
                debug!("Loaded admin user default profile from daemon");
                profile
            }
            Err(e) => {
                warn!(error = %e, "Failed to get admin user profile default, using hard-coded defaults");
                UserProfileConfig::full_default()
            }
        };

        // Query daemon for all admin user profiles
        let admin_user_profiles: HashMap<String, UserProfileConfig> = match daemon
            .list_user_profiles()
            .await
        {
            Ok(profiles) => {
                debug!(
                    count = profiles.len(),
                    "Loaded admin user profiles from daemon"
                );
                profiles
                    .into_iter()
                    .filter_map(|(name, json)| serde_json::from_str(&json).ok().map(|p| (name, p)))
                    .collect()
            }
            Err(e) => {
                warn!(error = %e, "Failed to get admin user profiles");
                HashMap::new()
            }
        };

        // Load user config (optional)
        let user_config = match user_config_path {
            Some(path) => match UserConfig::load_from(path) {
                Ok(Some(config)) => {
                    info!(
                        profiles = config.user.profiles.len(),
                        tv_assignments = config.tv.len(),
                        path = %path.display(),
                        "Loaded user configuration"
                    );
                    config
                }
                Ok(None) => {
                    debug!(path = %path.display(), "No user config file, using system defaults");
                    UserConfig::default()
                }
                Err(e) => {
                    warn!(error = %e, "Failed to load user config, using system defaults");
                    UserConfig::default()
                }
            },
            None => match UserConfig::load() {
                Ok(Some(config)) => {
                    info!(
                        profiles = config.user.profiles.len(),
                        tv_assignments = config.tv.len(),
                        "Loaded user configuration"
                    );
                    config
                }
                Ok(None) => {
                    debug!("No user config file, using system defaults");
                    UserConfig::default()
                }
                Err(e) => {
                    warn!(error = %e, "Failed to load user config, using system defaults");
                    UserConfig::default()
                }
            },
        };

        // Validate TV references in user config
        for tv_entry in &user_config.tv {
            if !tv_ids.contains(&tv_entry.uuid) {
                warn!(
                    tv_id = %tv_entry.uuid,
                    "User config references unknown TV (not in daemon's TV list)"
                );
            }
        }

        // Resolve user profiles for each TV.
        let mut profiles = HashMap::new();
        for tv_info in &tv_infos {
            let tv_id = &tv_info.uuid;

            let resolved = user_config.resolve_user_profile(tv_id, &admin_user_default);
            debug!(
                tv_id = %tv_id,
                profile = user_config.get_tv_user_profile_name(tv_id),
                idle_timeout = resolved.idle_timeout_seconds,
                "Resolved user profile"
            );
            profiles.insert(tv_id.clone(), resolved);
        }

        info!(tvs = tv_ids.len(), "Session state loaded");

        Ok(Self {
            profiles,
            user_config,
            admin_user_default,
            admin_user_profiles,
            tv_ids,
        })
    }

    /// Get the effective idle timeout (minimum non-zero across all TV profiles).
    ///
    /// # Design tradeoff: single shared timer
    ///
    /// A single `IdleMonitor` fires at the minimum non-zero timeout across all TVs.
    /// When `IdleTimeout` fires, `get_action_for_event` applies *each TV's own*
    /// `on_idle_timeout` action — so a TV with a 30-minute timeout won't do anything
    /// extra, but it *will* receive the event at 5 minutes if another TV has a shorter
    /// timeout. Per-TV timers (multiple `IdleMonitor` instances) would be more precise
    /// but add significant complexity for a marginal gain in the common single-TV case.
    pub fn effective_idle_timeout(&self) -> u64 {
        self.profiles
            .values()
            .map(|p| p.idle_timeout_seconds)
            .filter(|&t| t > 0)
            .min()
            .unwrap_or(0)
    }
}

/// Drop user-config `[[tv]]` entries whose UUID is no longer in the
/// daemon's TV list, persisting the pruned config to disk.
///
/// Called from a task that watches `DaemonClient::tv_list_change_stream`:
/// when the daemon unregisters a TV's `Tv1.*` interfaces (via
/// `Tv1.Remove()`), this reconciles the user's `[[tv]] uuid = "…" →
/// profile.user = "…"` assignments against the new authoritative
/// list. Without it, removing a TV from the GUI/CLI leaves orphan
/// entries the session warns about but never cleans up.
///
/// Uses the daemon's `list_tvs()` as the source of truth rather than
/// the signal payload itself — the signal carries an object path
/// component (underscored UUID) that would need decoding back to
/// the canonical form, and the daemon's list is the only place the
/// canonical UUID set lives without ambiguity.
///
/// On any error fetching the daemon's list (network, daemon
/// unavailable, etc.) this is a no-op so a transient failure
/// doesn't accidentally prune the user's assignments.
pub async fn reconcile_user_config_orphans(
    state: &Arc<RwLock<SessionState>>,
    daemon: &DaemonClient,
    config_path: Option<&PathBuf>,
) {
    let current_uuids: HashSet<String> = match daemon.list_tvs().await {
        Ok(tvs) => tvs.into_iter().map(|t| t.uuid).collect(),
        Err(e) => {
            warn!(error = %e, "Failed to query daemon TV list; skipping orphan prune");
            return;
        }
    };

    let mut s = state.write().await;
    let before = s.user_config.tv.len();
    s.user_config.tv.retain(|t| current_uuids.contains(&t.uuid));
    let pruned = before - s.user_config.tv.len();

    // Always refresh the cached daemon view, even if no orphans were
    // pruned — a TV pairing also fires this signal, and the session's
    // tv_ids/profiles caches need to grow accordingly.
    s.tv_ids = current_uuids.iter().cloned().collect();
    s.profiles.retain(|tv_id, _| current_uuids.contains(tv_id));

    if pruned == 0 {
        return;
    }

    let save_result = match config_path {
        Some(path) => s.user_config.save_to(path),
        None => s.user_config.save(),
    };
    match save_result {
        Ok(()) => info!(
            pruned = pruned,
            "Pruned orphan TV assignments from user config"
        ),
        Err(e) => warn!(
            error = %e,
            pruned = pruned,
            "Pruned orphan TV assignments in memory but failed to persist user config"
        ),
    }
}

/// Get the resolved event action for a session event from a user profile.
///
/// Returns the full [`ResolvedEventAction`] (action + qos) so the caller
/// can branch on the reliability tier when dispatching.
pub fn get_action_for_event(
    event: &session_monitor::SessionEvent,
    profile: &ResolvedUserProfile,
) -> ResolvedEventAction {
    match event {
        session_monitor::SessionEvent::SessionLocked => profile.on_session_lock,
        session_monitor::SessionEvent::SessionUnlocked => profile.on_session_unlock,
        session_monitor::SessionEvent::GreeterWake => profile.on_greeter_wake,
        session_monitor::SessionEvent::IdleTimeout => profile.on_idle_timeout,
        session_monitor::SessionEvent::IdleResume => profile.on_idle_resume,
    }
}

/// Execute an event action on a specific TV.
///
/// Thin shim that forwards the resolved `EventAction` (action + qos)
/// to the daemon via [`DaemonClient::dispatch_action`]. The
/// verify-retry-escalate state machine lives daemon-side in
/// `tv_manager::dispatch_reliable_screen_on` — see
/// `docs/plans/qos-dispatch-refactor.md` for the architectural
/// rationale.
pub async fn execute_action_for_tv(
    daemon: &DaemonClient,
    tv_id: &str,
    event_action: ResolvedEventAction,
) -> anyhow::Result<()> {
    if event_action.action == SessionAction::None {
        return Ok(());
    }
    let qos = QosConfig {
        reliability: Some(event_action.qos.reliability),
    };
    daemon
        .dispatch_action(tv_id, event_action.action, qos)
        .await
}

/// Handle session events by executing configured actions.
///
/// For each event, iterates through all TVs and applies the resolved
/// event action defined in each TV's profile. Different TVs can respond
/// differently to the same event (e.g., gaming TV ignores session lock).
///
/// Tracks the last [`SessionAction`] sent per TV to skip redundant
/// commands. Dedup is keyed on the action alone, not the QoS tier — if
/// the TV is already in the target state, re-dispatching at a higher
/// reliability tier wouldn't change anything.
pub async fn event_handler(
    mut event_rx: mpsc::UnboundedReceiver<session_monitor::SessionEvent>,
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
) {
    info!("Event handler started");

    let mut last_action: HashMap<String, SessionAction> = HashMap::new();

    while let Some(event) = event_rx.recv().await {
        info!(event = %event, "Processing session event");

        // Query current automated set from daemon (live, not cached).
        let automated_ids: HashSet<String> = match daemon.list_tvs().await {
            Ok(tvs) => tvs
                .into_iter()
                .filter(|t| t.automated)
                .map(|t| t.uuid)
                .collect(),
            Err(e) => {
                warn!(error = %e, "Failed to query automated TVs; skipping event dispatch");
                HashSet::new()
            }
        };

        let tv_actions: Vec<(String, ResolvedEventAction)> = {
            let s = state.read().await;
            s.tv_ids
                .iter()
                .filter(|tv_id| automated_ids.contains(*tv_id))
                .filter_map(|tv_id| {
                    let profile = s.profiles.get(tv_id)?;
                    let event_action = get_action_for_event(&event, profile);
                    Some((tv_id.clone(), event_action))
                })
                .collect()
        };

        let mut success_count = 0;
        for (tv_id, event_action) in tv_actions {
            if event_action.action == SessionAction::None {
                debug!(tv_id = %tv_id, "Action is None, skipping");
                continue;
            }

            if let Some(&prev) = last_action.get(&tv_id) {
                if prev == event_action.action {
                    debug!(
                        tv_id = %tv_id,
                        action = ?event_action.action,
                        "Skipping redundant action",
                    );
                    success_count += 1;
                    continue;
                }
            }

            debug!(
                tv_id = %tv_id,
                action = ?event_action.action,
                reliability = ?event_action.qos.reliability,
                "Executing action",
            );
            if let Err(e) = execute_action_for_tv(&daemon, &tv_id, event_action).await {
                warn!(tv_id = %tv_id, error = %e, "Action failed");
                last_action.remove(&tv_id);
            } else {
                last_action.insert(tv_id, event_action.action);
                success_count += 1;
            }
        }

        if success_count > 0 {
            info!(event = %event, count = success_count, "Event processed");
        }
    }

    info!("Event handler stopped");
}
