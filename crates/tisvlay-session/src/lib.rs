//! tisvlay-session library — user session service logic.
//!
//! The binary entry point lives in `main.rs`; this library exposes the
//! modules that integration tests need to drive.

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
    ActionExecutor, DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, DaemonClient,
    ResolvedUserProfile, SessionAction, TvAssignment, UserConfig, UserProfileBody,
    UserProfileConfig, user_references_of,
};
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

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
                profile = %user_config.get_tv_user_profile(tv_id),
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
    /// A single `ext-idle-notify-v1` idle notification fires at the minimum
    /// non-zero timeout across all TVs. When `IdleTimeout` fires,
    /// `get_action_for_event` applies *each TV's own* `on_idle_timeout` action —
    /// so a TV with a 30-minute timeout won't do anything extra, but it *will*
    /// receive the event at 5 minutes if another TV has a shorter timeout. Per-TV
    /// notifications would be more precise but add significant complexity for a
    /// marginal gain in the common single-TV case.
    ///
    /// The daemon has a deliberate parallel for the login greeter
    /// (`TvManager::greeter_idle_timeout_seconds` + its `GreeterIdle`/`GreeterResume`
    /// fan-out): the same single-shared-clock-plus-per-TV-action shape over the
    /// system profile. They share no code; keep the shared-clock semantics in sync.
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

/// Get the action for a session event from a resolved user profile.
pub fn get_action_for_event(
    event: &session_monitor::SessionEvent,
    profile: &ResolvedUserProfile,
) -> SessionAction {
    match event {
        session_monitor::SessionEvent::SessionLocked => profile.on_session_lock,
        session_monitor::SessionEvent::SessionUnlocked => profile.on_session_unlock,
        session_monitor::SessionEvent::LockWake => profile.on_lock_wake,
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
    daemon.apply_action(action, tv_id).await
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

    // No `last_action` dedup. Every action self-verifies in
    // `tv_manager` via its pre-flight check against
    // `last_known_power_state`, so a duplicate dispatch is at worst
    // a fast no-op when cached state already matches the target.
    // Removing the dedup avoids reasoning about edge cases where
    // (e.g.) a TV powered off out-of-band between two same-event
    // firings would silently skip the second.

    while let Some(event) = event_rx.recv().await {
        info!(event = %event, "Processing session event: {event}");

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

            info!(tv_id = %tv_id, action = ?action, "Dispatching session action: {action:?}");
            if let Err(e) = execute_action_for_tv(&daemon, &tv_id, action).await {
                warn!(tv_id = %tv_id, error = %e, "Action failed");
            } else {
                success_count += 1;
            }
        }

        if success_count > 0 {
            info!(event = %event, count = success_count, "Event processed");
        }
    }

    info!("Event handler stopped");
}

/// Supervise the Wayland presence watch so an in-session idle-timeout change
/// (via the GUI, a D-Bus `ReloadConfig`, or SIGHUP — all of which update
/// [`SessionState`]) is applied **without a session restart**. The watch
/// registers its `ext-idle-notify-v1` notification with the timeout captured at
/// start, so re-registering means restarting it: this task polls the effective
/// timeout and, when it changes, drops the old [`WatchHandle`](wayland_idle::WatchHandle)
/// (stopping the watch + its policy task) and starts a fresh watch with the new
/// timeout. The lock-wake notification (fixed at 200 ms) is re-registered too,
/// transparently. Polling — rather than threading a notification through every
/// profile mutator — keeps this self-contained and robust to any change source;
/// the change applies within one poll interval, effectively live for a setting
/// that changes rarely. It also retries a failed start and restarts the watch
/// when the compositor disconnects, so a transient Wayland-unavailable at
/// launch or a mid-session compositor restart doesn't silently disable presence
/// detection until the next config edit.
pub async fn idle_watch_supervisor(
    event_tx: mpsc::UnboundedSender<session_monitor::SessionEvent>,
    is_locked: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<RwLock<SessionState>>,
) {
    const POLL: std::time::Duration = std::time::Duration::from_secs(2);

    let effective_ms = |s: &SessionState| -> Option<u32> {
        let secs = s.effective_idle_timeout();
        (secs > 0).then(|| secs.saturating_mul(1000).min(u32::MAX as u64) as u32)
    };
    // Returns the handle (kept to hold the watch open) and a receiver that fires
    // if the watch thread dies. A `None` handle means the start failed (Wayland
    // not up yet / X11) — retried on the next poll tick.
    let start = |timeout_ms: Option<u32>| match wayland_idle::start_wayland_idle_monitor(
        event_tx.clone(),
        is_locked.clone(),
        timeout_ms,
    ) {
        Ok((handle, dead)) => {
            info!(idle_timeout_ms = ?timeout_ms, "Wayland presence detection enabled");
            (Some(handle), Some(dead))
        }
        Err(e) => {
            warn!("Wayland presence detection unavailable: {}", e);
            (None, None)
        }
    };

    let mut current = effective_ms(&*state.read().await);
    let (mut handle, mut dead_rx) = start(current);
    let mut ticker = tokio::time::interval(POLL);
    loop {
        // Wake on a poll tick (timeout-change / retry) or the watch dying.
        let died = tokio::select! {
            _ = ticker.tick() => false,
            _ = wait_watch_dead(&mut dead_rx) => true,
        };
        if died {
            // A compositor restart / disconnect killed the watch. Restart it so
            // lock-wake and idle don't silently stop until a config edit.
            warn!("Wayland presence watch ended (compositor gone?); restarting");
            drop(handle.take());
            (handle, dead_rx) = start(current);
            continue;
        }
        let next = effective_ms(&*state.read().await);
        if next != current {
            info!(
                old_ms = ?current,
                new_ms = ?next,
                "in-session idle timeout changed; re-registering Wayland watch",
            );
            // Drop the old handle first so the old watch stops before the new
            // one binds (avoids a momentary double notification).
            drop(handle.take());
            (handle, dead_rx) = start(next);
            current = next;
        } else if handle.is_none() {
            // A prior start failed (Wayland not up yet at login, or a restart
            // left no watch). Retry so presence detection recovers without
            // waiting for a config edit.
            debug!("Retrying Wayland presence watch");
            (handle, dead_rx) = start(current);
        }
    }
}

/// Await the watch-died signal, or pend forever when no watch is running (so the
/// `select!` falls through to the poll tick, which retries the start).
async fn wait_watch_dead(dead_rx: &mut Option<oneshot::Receiver<()>>) {
    match dead_rx.as_mut() {
        Some(rx) => {
            let _ = rx.await;
        }
        None => std::future::pending::<()>().await,
    }
}

// =============================================================================
// User-profile CRUD
//
// Session-side mirror of the daemon's system-profile CRUD shape. The four
// mutators (`create`, `duplicate`, `update`, `delete`) follow the same
// validate → mutate in-memory → persist → rollback-on-IO-failure pattern.
// Persistence is gated by `disk_write_lock` so concurrent CRUD calls
// serialize against each other (matches `set_tv_user_profile`'s save path).
//
// None of these helpers emit D-Bus signals directly — the `Session1`
// handlers do that after a successful return so the catalog is guaranteed
// fetchable by the moment a client receives `Added` / `Changed` / `Removed`.
// =============================================================================

/// Error type for user-profile CRUD operations on [`SessionState`].
///
/// Mirrors the daemon-side `TvManagerError` profile variants so the
/// `Session1` D-Bus error mapping can map onto the same wire names
/// (`org.tisvlay.Session1.Error.ProfileNotFound`, etc.).
#[derive(Debug, thiserror::Error)]
pub enum UserProfileError {
    /// Target profile UUID not present in the catalog.
    #[error("Profile not found: {0}")]
    NotFound(String),
    /// Attempt to delete or otherwise mutate a reserved profile in a
    /// way the session doesn't allow (currently: deleting
    /// [`DEFAULT_USER_PROFILE_UUID`]).
    #[error("Reserved profile cannot be modified: {0}")]
    Reserved(String),
    /// Body's `name` is empty, claims the reserved `"default"` literal
    /// for a non-default UUID, or collides with an existing profile.
    #[error("Profile name invalid: {0}")]
    NameInvalid(String),
    /// At least one TV still references the target via
    /// `UserTvConfig.profile.user`.
    #[error("Profile still referenced by {count} TV(s): {profile_uuid}")]
    Referenced { profile_uuid: String, count: usize },
    /// Persistence failed after the in-memory mutation already landed.
    /// Callers roll the in-memory state back before propagating.
    #[error("I/O error: {0}")]
    Io(String),
}

impl SessionState {
    /// Resolve a user-profile body to its dict-derive wire shape.
    fn user_body_for_uuid(&self, uuid: Uuid) -> Option<UserProfileBody> {
        self.user_config
            .user
            .profiles
            .get(&uuid)
            .map(UserProfileBody::from_config)
    }

    /// Snapshot of the user-profile catalog suitable for
    /// `Session1.ListUserProfiles -> a{sa{sv}}`. The session merges the
    /// admin baseline (from the daemon) with user-defined bodies — the
    /// admin baseline always supplies the reserved default UUID even
    /// when no user-defined body exists.
    pub fn list_user_profile_bodies(&self) -> Vec<(Uuid, UserProfileBody)> {
        let mut merged: std::collections::BTreeMap<Uuid, UserProfileBody> =
            std::collections::BTreeMap::new();

        // Admin baseline: every admin user profile flows through under
        // the reserved default UUID. The session only knows about names
        // for admin profiles, so the daemon-supplied admin default
        // becomes the body for [`DEFAULT_USER_PROFILE_UUID`].
        let admin_default = UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..self.admin_user_default.clone()
        };
        merged.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileBody::from_config(&admin_default),
        );

        // User-defined profiles override the admin baseline. The
        // catalog is UUID-keyed, so a user-defined default-UUID body
        // displaces the admin baseline cleanly.
        for (uuid, body) in &self.user_config.user.profiles {
            merged.insert(*uuid, UserProfileBody::from_config(body));
        }

        merged.into_iter().collect()
    }

    /// Hard-coded baseline body — every user profile inherits from this
    /// when its fields are all `None`. Static; mirrors
    /// [`tisvlay_core::ResolvedUserProfile::default`] as a wire body.
    pub fn resolved_user_profile_defaults() -> UserProfileBody {
        UserProfileBody::from_config(&UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..UserProfileConfig::full_default()
        })
    }

    /// Fetch one body by UUID. Returns
    /// [`UserProfileError::NotFound`] for absent UUIDs. The reserved
    /// default UUID is always fetchable — it surfaces the admin
    /// baseline body when no user body exists for it.
    pub fn get_user_profile_body(&self, uuid: Uuid) -> Result<UserProfileBody, UserProfileError> {
        if let Some(body) = self.user_body_for_uuid(uuid) {
            return Ok(body);
        }
        if uuid == DEFAULT_USER_PROFILE_UUID {
            // Fall through to the admin baseline — the user's catalog
            // doesn't have a body under the default UUID yet, but the
            // wire surface still owes the caller something.
            let admin_default = UserProfileConfig {
                name: DEFAULT_PROFILE_NAME.to_string(),
                ..self.admin_user_default.clone()
            };
            return Ok(UserProfileBody::from_config(&admin_default));
        }
        Err(UserProfileError::NotFound(uuid.to_string()))
    }

    /// Resolve the per-TV walk to a fully-populated body. Mirrors
    /// `Manager1.GetEffectiveSystemProfile`: per-TV body → user
    /// default body → admin baseline body → hardcoded defaults. Every
    /// `Option<T>` in the returned body is `Some`.
    pub fn get_effective_user_profile_body(&self, tv_id: &str) -> UserProfileBody {
        let resolved = self
            .user_config
            .resolve_user_profile(tv_id, &self.admin_user_default);
        let assigned_uuid = self.user_config.get_tv_user_profile(tv_id);
        // Surface the assigned profile's display name when available.
        // Falls back to the reserved-default name when the catalog has
        // no body for this UUID — same convention as the system side.
        let display_name = self
            .user_config
            .user
            .profiles
            .get(&assigned_uuid)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_string());
        UserProfileBody {
            name: display_name,
            on_session_lock: Some(resolved.on_session_lock),
            on_session_unlock: Some(resolved.on_session_unlock),
            on_lock_wake: Some(resolved.on_lock_wake),
            on_idle_timeout: Some(resolved.on_idle_timeout),
            on_idle_resume: Some(resolved.on_idle_resume),
            idle_timeout_seconds: Some(resolved.idle_timeout_seconds),
        }
    }

    /// Collect lightweight per-TV user-profile assignments suitable
    /// for [`user_references_of`] walks.
    fn user_assignments_for_refs(&self) -> Vec<TvAssignment> {
        let mut out = Vec::with_capacity(self.user_config.tv.len());
        for entry in &self.user_config.tv {
            let Ok(parsed) = Uuid::parse_str(&entry.uuid) else {
                continue;
            };
            out.push(TvAssignment {
                tv_uuid: parsed,
                system_profile: None,
                user_profile: Some(entry.profile.user),
            });
        }
        out
    }
}

/// Persist a user config snapshot to disk, with fallback to the XDG
/// default when no explicit path is configured. Returns
/// `UserProfileError::Io` carrying the failure detail.
fn persist_user_config(
    config: &UserConfig,
    config_path: Option<&PathBuf>,
) -> Result<(), UserProfileError> {
    let result = match config_path {
        Some(path) => config.save_to(path),
        None => config.save(),
    };
    result.map_err(|e| UserProfileError::Io(format!("Failed to save user config: {e}")))
}

/// Create a new user profile with the given display name. Validates
/// `name` (non-empty, not the reserved `"default"` literal, unique
/// across the catalog body names), mints a fresh UUID, inserts a
/// minimal body, persists, and returns the new UUID.
///
/// On persistence failure the in-memory insert is rolled back; no
/// caller-side cleanup needed beyond mapping the error onto the wire.
pub async fn create_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    name: &str,
) -> Result<Uuid, UserProfileError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(UserProfileError::NameInvalid(
            "name must be non-empty".to_string(),
        ));
    }
    if trimmed == DEFAULT_PROFILE_NAME {
        return Err(UserProfileError::NameInvalid(format!(
            "\"{DEFAULT_PROFILE_NAME}\" is reserved for the default profile"
        )));
    }

    let mut s = state.write().await;
    if s.user_config
        .user
        .profiles
        .values()
        .any(|p| p.name == trimmed)
    {
        return Err(UserProfileError::NameInvalid(format!(
            "name \"{trimmed}\" already in use"
        )));
    }

    let new_uuid = Uuid::new_v4();
    if s.user_config.user.profiles.contains_key(&new_uuid) {
        return Err(UserProfileError::Io(
            "UUID collision on new profile (retry)".to_string(),
        ));
    }
    let new_body = UserProfileConfig {
        name: trimmed.to_string(),
        ..UserProfileConfig::default()
    };
    s.user_config.user.profiles.insert(new_uuid, new_body);

    if let Err(e) = persist_user_config(&s.user_config, config_path) {
        s.user_config.user.profiles.remove(&new_uuid);
        return Err(e);
    }

    info!(profile_uuid = %new_uuid, name = %trimmed, "User profile created");
    Ok(new_uuid)
}

/// Duplicate an existing user profile under a new display name. Same
/// validation rules as [`create_user_profile`] — the new body inherits
/// every field besides `name` from the source.
pub async fn duplicate_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    source_uuid: Uuid,
    new_name: &str,
) -> Result<Uuid, UserProfileError> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err(UserProfileError::NameInvalid(
            "name must be non-empty".to_string(),
        ));
    }
    if trimmed == DEFAULT_PROFILE_NAME {
        return Err(UserProfileError::NameInvalid(format!(
            "\"{DEFAULT_PROFILE_NAME}\" is reserved for the default profile"
        )));
    }

    let mut s = state.write().await;
    let source = s
        .user_config
        .user
        .profiles
        .get(&source_uuid)
        .cloned()
        .ok_or_else(|| UserProfileError::NotFound(source_uuid.to_string()))?;
    if s.user_config
        .user
        .profiles
        .values()
        .any(|p| p.name == trimmed)
    {
        return Err(UserProfileError::NameInvalid(format!(
            "name \"{trimmed}\" already in use"
        )));
    }

    let new_uuid = Uuid::new_v4();
    if s.user_config.user.profiles.contains_key(&new_uuid) {
        return Err(UserProfileError::Io(
            "UUID collision on new profile (retry)".to_string(),
        ));
    }
    let mut cloned = source;
    cloned.name = trimmed.to_string();
    s.user_config.user.profiles.insert(new_uuid, cloned);

    if let Err(e) = persist_user_config(&s.user_config, config_path) {
        s.user_config.user.profiles.remove(&new_uuid);
        return Err(e);
    }

    info!(
        profile_uuid = %new_uuid,
        source_uuid = %source_uuid,
        name = %trimmed,
        "User profile duplicated"
    );
    Ok(new_uuid)
}

/// Delete a user profile. Rejects [`DEFAULT_USER_PROFILE_UUID`]
/// outright; rejects when at least one `[[tv]] profile.user` still
/// references the target. Persistence failure rolls the in-memory
/// removal back.
pub async fn delete_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
) -> Result<(), UserProfileError> {
    if uuid == DEFAULT_USER_PROFILE_UUID {
        return Err(UserProfileError::Reserved(uuid.to_string()));
    }

    let mut s = state.write().await;

    let referencing = {
        let assignments = s.user_assignments_for_refs();
        user_references_of(uuid, &assignments)
    };
    if !referencing.is_empty() {
        return Err(UserProfileError::Referenced {
            profile_uuid: uuid.to_string(),
            count: referencing.len(),
        });
    }

    let removed_body = match s.user_config.user.profiles.remove(&uuid) {
        Some(b) => b,
        None => return Err(UserProfileError::NotFound(uuid.to_string())),
    };

    if let Err(e) = persist_user_config(&s.user_config, config_path) {
        s.user_config.user.profiles.insert(uuid, removed_body);
        return Err(e);
    }

    info!(profile_uuid = %uuid, "User profile deleted");
    Ok(())
}

/// Atomic full-body replace for an existing user profile. Validates
/// `body.name` (non-empty, unique across the catalog excluding the
/// same UUID, default-name reserved for the default UUID). Overwrites
/// the catalog entry wholesale via `UserProfileConfig::from(&body)`.
/// Persistence failure restores the previous body.
///
/// A successful update also refreshes the resolved-profile cache for
/// every TV — body changes shift inheritance for TVs that point at
/// this UUID (or, when `uuid` is the default, every TV with no
/// explicit assignment).
pub async fn update_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
    body: UserProfileBody,
) -> Result<(), UserProfileError> {
    let trimmed = body.name.trim();
    if trimmed.is_empty() {
        return Err(UserProfileError::NameInvalid(
            "name must be non-empty".to_string(),
        ));
    }
    if uuid == DEFAULT_USER_PROFILE_UUID {
        if trimmed != DEFAULT_PROFILE_NAME {
            return Err(UserProfileError::NameInvalid(format!(
                "default profile's name must be \"{DEFAULT_PROFILE_NAME}\""
            )));
        }
    } else if trimmed == DEFAULT_PROFILE_NAME {
        return Err(UserProfileError::NameInvalid(format!(
            "\"{DEFAULT_PROFILE_NAME}\" is reserved for the default profile"
        )));
    }

    let mut s = state.write().await;

    // The default UUID is always updatable — even if no body exists
    // for it in the user catalog yet, an update materializes one.
    if uuid != DEFAULT_USER_PROFILE_UUID && !s.user_config.user.profiles.contains_key(&uuid) {
        return Err(UserProfileError::NotFound(uuid.to_string()));
    }
    if s.user_config
        .user
        .profiles
        .iter()
        .any(|(other, p)| *other != uuid && p.name == trimmed)
    {
        return Err(UserProfileError::NameInvalid(format!(
            "name \"{trimmed}\" already in use"
        )));
    }

    let mut new_config = UserProfileConfig::from(&body);
    new_config.name = trimmed.to_string();
    let prior_body = s.user_config.user.profiles.insert(uuid, new_config);

    if let Err(e) = persist_user_config(&s.user_config, config_path) {
        match prior_body {
            Some(prev) => {
                s.user_config.user.profiles.insert(uuid, prev);
            }
            None => {
                s.user_config.user.profiles.remove(&uuid);
            }
        }
        return Err(e);
    }

    // Refresh resolved-profile cache for every TV; body change may
    // shift inheritance for any TV that points at this UUID.
    let admin_default = s.admin_user_default.clone();
    let user_config_snapshot = s.user_config.clone();
    let tv_ids: Vec<String> = s.tv_ids.clone();
    for tv_id in &tv_ids {
        let resolved = user_config_snapshot.resolve_user_profile(tv_id, &admin_default);
        s.profiles.insert(tv_id.clone(), resolved);
    }

    info!(profile_uuid = %uuid, name = %trimmed, "User profile updated");
    Ok(())
}
