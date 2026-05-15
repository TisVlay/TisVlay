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
use std::time::Duration;
use tisvlay_core::{
    DaemonClient, Reliability, ResolvedEventAction, ResolvedUserProfile, SessionAction, UserConfig,
    UserProfileConfig,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, warn};

/// PowerState string the LG backend pushes when the TV's panel is fully on.
///
/// Matches [`tisvlay_core::PowerState::Active`]'s `Display` form. Used by
/// the reliable dispatch helper to verify a `screen_on` reached the panel
/// rather than landing in `screen_off` or a deeper power state.
const POWER_STATE_ACTIVE: &str = "active";

/// Delay between dispatching a wake-side action and reading PowerState back.
///
/// LG's `getPowerState` subscription is typically sub-100 ms; 300 ms gives
/// the subscription a comfortable window to refresh before we verify.
const RELIABLE_VERIFY_DELAY: Duration = Duration::from_millis(300);

/// Max `screen_on` attempts before the reliable path escalates to `power_on`.
///
/// Two attempts cover the racy "TV processed the request but the
/// PowerState push lagged" case; further retries are pointless once we've
/// concluded the TV slipped past Screen Off, because `screen_on` returns
/// `-102` ("not in screen-off substate") and only WoL can recover.
const RELIABLE_SCREEN_ON_ATTEMPTS: usize = 2;

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
/// Branches on `event_action.qos.reliability`:
/// - [`Reliability::BestEffort`] dispatches the action once and returns.
/// - [`Reliability::Reliable`] on `screen_on` verifies the TV reached the
///   `active` PowerState; if it slipped past Screen Off, escalates to
///   `power_on` (which has WoL + retry internally). Other actions don't
///   currently benefit from the reliable path (sleep-side fire-and-forget;
///   `power_on` already retries internally), so they fall through to the
///   single dispatch.
pub async fn execute_action_for_tv(
    daemon: &DaemonClient,
    tv_id: &str,
    event_action: ResolvedEventAction,
) -> anyhow::Result<()> {
    match (event_action.action, event_action.qos.reliability) {
        (SessionAction::None, _) => Ok(()),
        (SessionAction::ScreenOn, Reliability::Reliable) => reliable_screen_on(daemon, tv_id).await,
        (SessionAction::ScreenOn, Reliability::BestEffort) => daemon.screen_on(tv_id).await,
        (SessionAction::ScreenOff, _) => daemon.screen_off(tv_id).await,
        (SessionAction::PowerOff, _) => daemon.power_off(tv_id).await,
        (SessionAction::PowerOn, _) => daemon.power_on(tv_id).await,
    }
}

/// Dispatch `screen_on` reliably: verify the TV reached `active`, escalate
/// to `power_on` (with WoL) if it slipped past Screen Off.
///
/// The retry budget covers the case where the SSAP `getPowerState`
/// subscription momentarily lags behind the panel transition; the
/// escalation covers the case the user is actually complaining about,
/// where the TV transitioned itself from Screen Off to a deeper state
/// while the desktop was locked.
async fn reliable_screen_on(daemon: &DaemonClient, tv_id: &str) -> anyhow::Result<()> {
    for attempt in 0..RELIABLE_SCREEN_ON_ATTEMPTS {
        if let Err(e) = daemon.screen_on(tv_id).await {
            // The daemon returning an error here usually means the
            // WebSocket is closed (TV unreachable). No point retrying
            // screen_on — escalate to power_on which has WoL inside.
            debug!(tv_id, attempt, error = %e, "screen_on failed; escalating to power_on");
            break;
        }
        tokio::time::sleep(RELIABLE_VERIFY_DELAY).await;
        match daemon.power_state(tv_id).await {
            Ok(state) if state == POWER_STATE_ACTIVE => return Ok(()),
            Ok(state) => {
                debug!(
                    tv_id,
                    attempt, %state,
                    "screen_on did not reach active state",
                );
            }
            Err(e) => {
                debug!(tv_id, attempt, error = %e, "failed to read power state");
            }
        }
    }
    info!(
        tv_id,
        "reliable screen_on escalating to power_on (TV likely slipped past Screen Off)",
    );
    daemon.power_on(tv_id).await
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
