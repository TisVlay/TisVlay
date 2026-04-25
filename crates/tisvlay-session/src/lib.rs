//! tisvlay-session library — user session service logic.
//!
//! The binary entry point lives in `main.rs`; this library exposes the
//! modules that integration tests need to drive.

pub mod desktop_probe;
pub mod idle;
pub mod session_dbus;
pub mod session_monitor;
pub mod wayland_idle;

/// Re-exports for the integration-test harness crate. Not public API.
#[doc(hidden)]
pub mod __internals {
    pub use crate::SessionState;
    pub use crate::event_handler;
    pub use crate::session_dbus::register_session_dbus_on;
    pub use crate::session_monitor::SessionEvent;
}

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{
    DaemonClient, DesktopMapping, HardwareId, ResolvedUserProfile, SessionAction, UserConfig,
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
        let tv_ids: Vec<String> = tv_infos.iter().map(|t| t.id.clone()).collect();

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
            if !tv_ids.contains(&tv_entry.id) {
                warn!(
                    tv_id = %tv_entry.id,
                    "User config references unknown TV (not in daemon's TV list)"
                );
            }
        }

        // Resolve user profiles for each TV.
        let mut profiles = HashMap::new();
        for tv_info in &tv_infos {
            let tv_id = &tv_info.id;

            let resolved = user_config.resolve_user_profile(tv_id, &admin_user_default);
            debug!(
                tv_id = %tv_id,
                profile = user_config.get_tv_user_profile_name(tv_id),
                idle_timeout = resolved.idle_timeout_seconds,
                "Resolved user profile"
            );
            profiles.insert(tv_id.clone(), resolved);
        }

        // Auto-detect desktop-side identity (EDID → HardwareId) for
        // each managed TV and push the resolved DesktopMapping to the
        // daemon so it can republish on Tv1.Desktop. In-memory only:
        // the probe re-runs at every session start and reload, so no
        // user-config persistence is needed for cross-restart
        // stability — the matcher itself is deterministic.
        probe_and_push_desktop_mappings(daemon, &tv_infos).await;

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

/// Run the DRM probe, match each managed TV to a physical connector
/// by EDID, and push a `DesktopMapping` per TV to the daemon.
///
/// Best-effort end-to-end: any individual step failing logs a warning
/// and moves on. A failed probe or a failed push doesn't block session
/// startup — the session service can still handle idle / session lock
/// events without the display mapping being wired up.
///
/// In-memory only — no on-disk persistence. The matcher is
/// deterministic (sorts probed displays by connector name and
/// greedily claims one per TV), so cross-restart stability follows
/// from the probe itself, not from cached state.
async fn probe_and_push_desktop_mappings(daemon: &DaemonClient, tv_infos: &[tisvlay_core::TvInfo]) {
    // Collect TVs with a non-empty HardwareId — only those can be
    // EDID-matched. Unmanaged TVs and headless TVs still get a
    // DesktopMapping push with empty identity fields below.
    let tvs_for_match: Vec<(String, HardwareId)> = tv_infos
        .iter()
        .filter(|t| !t.hardware_id.is_empty())
        .map(|t| (t.id.clone(), t.hardware_id.clone()))
        .collect();

    let probed = desktop_probe::ProbedDisplay::enumerate();
    debug!(
        count = probed.len(),
        "Desktop probe enumerated DRM displays"
    );

    let matches = desktop_probe::match_displays_to_tvs(&probed, &tvs_for_match);

    for tv in tv_infos {
        let mapping = match matches.get(&tv.id) {
            Some(d) => DesktopMapping {
                edid: d.edid_hash.clone(),
                connector: d.connector.clone(),
            },
            None => DesktopMapping::default(),
        };
        if let Err(e) = daemon.set_desktop_mapping(&tv.id, mapping).await {
            warn!(tv_id = %tv.id, error = %e, "Failed to push desktop mapping to daemon");
        }
    }
}

/// Get the action for a session event from a resolved user profile.
pub fn get_action_for_event(
    event: &session_monitor::SessionEvent,
    profile: &ResolvedUserProfile,
) -> SessionAction {
    match event {
        session_monitor::SessionEvent::SessionLocked => profile.on_session_lock,
        session_monitor::SessionEvent::SessionUnlocked => profile.on_session_unlock,
        session_monitor::SessionEvent::GreeterWake => profile.on_greeter_wake,
        session_monitor::SessionEvent::IdleTimeout => profile.on_idle_timeout,
        session_monitor::SessionEvent::IdleResume => profile.on_idle_resume,
    }
}

/// Execute an action on a specific TV.
pub async fn execute_action_for_tv(
    daemon: &DaemonClient,
    tv_id: &str,
    action: SessionAction,
) -> anyhow::Result<()> {
    match action {
        SessionAction::ScreenOff => daemon.screen_off(tv_id).await,
        SessionAction::ScreenOn => daemon.screen_on(tv_id).await,
        SessionAction::PowerOff => daemon.power_off(tv_id).await,
        SessionAction::PowerOn => daemon.power_on(tv_id).await,
        SessionAction::None => Ok(()),
    }
}

/// Handle session events by executing configured actions.
///
/// For each event, iterates through all TVs and applies the action
/// defined in each TV's resolved profile. Different TVs can respond
/// differently to the same event (e.g., gaming TV ignores session lock).
///
/// Tracks the last action sent to each TV to avoid redundant commands.
pub async fn event_handler(
    mut event_rx: mpsc::UnboundedReceiver<session_monitor::SessionEvent>,
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
) {
    info!("Event handler started");

    // Track last successful action per TV to skip redundant commands
    let mut last_action: HashMap<String, SessionAction> = HashMap::new();

    while let Some(event) = event_rx.recv().await {
        info!(event = %event, "Processing session event");

        // Query current automated set from daemon (live, not cached).
        let automated_ids: HashSet<String> = match daemon.list_tvs().await {
            Ok(tvs) => tvs
                .into_iter()
                .filter(|t| t.automated)
                .map(|t| t.id)
                .collect(),
            Err(e) => {
                warn!(error = %e, "Failed to query automated TVs; skipping event dispatch");
                HashSet::new()
            }
        };

        // Get TV actions from resolved profiles, filtered to currently managed TVs
        let tv_actions: Vec<(String, SessionAction)> = {
            let s = state.read().await;
            s.tv_ids
                .iter()
                .filter(|tv_id| automated_ids.contains(*tv_id))
                .filter_map(|tv_id| {
                    let profile = s.profiles.get(tv_id)?;
                    let action = get_action_for_event(&event, profile);
                    Some((tv_id.clone(), action))
                })
                .collect()
        };

        // Execute action for each TV
        let mut success_count = 0;
        for (tv_id, action) in tv_actions {
            if action == SessionAction::None {
                debug!(tv_id = %tv_id, "Action is None, skipping");
                continue;
            }

            // Skip redundant commands
            if let Some(&prev) = last_action.get(&tv_id) {
                if prev == action {
                    debug!(tv_id = %tv_id, action = ?action, "Skipping redundant action");
                    success_count += 1;
                    continue;
                }
            }

            debug!(tv_id = %tv_id, action = ?action, "Executing action");
            if let Err(e) = execute_action_for_tv(&daemon, &tv_id, action).await {
                warn!(tv_id = %tv_id, error = %e, "Action failed");
                last_action.remove(&tv_id);
            } else {
                last_action.insert(tv_id, action);
                success_count += 1;
            }
        }

        if success_count > 0 {
            info!(event = %event, count = success_count, "Event processed");
        }
    }

    info!("Event handler stopped");
}
