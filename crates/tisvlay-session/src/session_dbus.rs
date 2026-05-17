//! D-Bus interface for the session service.
//!
//! Exposes `org.tisvlay.Session1` on the session bus for profile management.
//! Used by the tray app and future GUI to query and change profile assignments.

use crate::SessionState;
use anyhow::Context;
use std::sync::Arc;
use tisvlay_core::DaemonClient;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use zbus::interface;
use zbus::object_server::SignalEmitter;

/// D-Bus name for the session service.
pub const SESSION_DBUS_NAME: &str = "org.tisvlay.Session";

/// D-Bus path for the session service.
pub const SESSION_PATH: &str = "/org/tisvlay/Session";

/// D-Bus error type for session service methods.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.tisvlay.Session1.Error")]
pub enum SessionError {
    /// Generic ZBus error.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Operation failed.
    OperationFailed(String),
    /// Profile not found.
    ProfileNotFound(String),
    /// `CreateUserProfile` called with a name that already exists
    /// in the user config (admin-side presence doesn't count —
    /// users may create overrides of admin profiles).
    ProfileExists(String),
    /// `name == "default"` is reserved against Delete (the merged
    /// default is always visible; users can shadow individual
    /// fields via Update but can't delete the concept).
    ProfileReserved(String),
    /// `DeleteUserProfile` called against a profile that one or
    /// more `[[tv]]` entries in the user config reference. Message
    /// format: `"<name>: <tv_id>, <tv_id>, ..."` so the GUI's
    /// delete-confirm dialog can split on `": "` and surface
    /// the offending TV ids.
    ProfileInUse(String),
}

/// D-Bus service for session profile management.
#[derive(Debug)]
pub struct SessionService {
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
    config_path: Option<std::path::PathBuf>,
}

impl SessionService {
    pub fn new(
        state: Arc<RwLock<SessionState>>,
        daemon: Arc<DaemonClient>,
        config_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            state,
            daemon,
            config_path,
        }
    }

    /// Persist the current `state.user_config` to the resolved
    /// user-config path (or XDG default if not overridden).
    ///
    /// Crate-internal helper for `create_user_profile` /
    /// `update_user_profile` / `delete_user_profile`. Returns a
    /// typed `SessionError::OperationFailed` on disk-write failure
    /// so the D-Bus caller gets a clear error instead of a silent
    /// in-memory-only success.
    fn save_user_config(&self, s: &SessionState) -> Result<(), SessionError> {
        let save_result = match &self.config_path {
            Some(path) => s.user_config.save_to(path),
            None => s.user_config.save(),
        };
        save_result.map_err(|e| {
            SessionError::OperationFailed(format!("Failed to save user config: {}", e))
        })
    }
}

#[interface(name = "org.tisvlay.Session1")]
impl SessionService {
    /// Compile-time version of the running session-service binary.
    /// Lets the GUI surface installed-version drift between processes
    /// after a partial upgrade where services weren't restarted.
    #[zbus(property(emits_changed_signal = "const"))]
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Resolved on-disk path of the user's config file. Falls back to
    /// the canonical XDG default (`~/.config/tisvlay/config.toml`)
    /// when `--config` wasn't passed; empty string only when path
    /// resolution fails entirely (no XDG dirs).
    #[zbus(property(emits_changed_signal = "const"))]
    fn config_path(&self) -> String {
        let resolved = self
            .config_path
            .clone()
            .or_else(|| tisvlay_core::UserConfig::config_path().ok());
        resolved
            .as_ref()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
            .to_string()
    }

    /// List all available user profiles.
    ///
    /// Returns: Array of (profile_name, profile_json) tuples.
    /// Includes user-defined profiles merged with admin profiles.
    /// User profiles take precedence over admin profiles with the same name.
    async fn list_user_profiles(&self) -> Result<Vec<(String, String)>, SessionError> {
        debug!("D-Bus: ListUserProfiles");
        let s = self.state.read().await;

        // Start with admin profiles as the base
        let mut merged: std::collections::HashMap<String, &tisvlay_core::UserProfileConfig> =
            std::collections::HashMap::new();
        for (name, profile) in &s.admin_user_profiles {
            merged.insert(name.clone(), profile);
        }

        // User profiles override admin profiles with the same name
        for (name, profile) in &s.user_config.user.profiles {
            merged.insert(name.clone(), profile);
        }

        // Ensure "default" is always present
        if !merged.contains_key("default") {
            merged.insert("default".to_string(), &s.admin_user_default);
        }

        let mut profiles = Vec::new();
        for (name, profile) in merged {
            let json = serde_json::to_string(profile).map_err(|e| {
                SessionError::OperationFailed(format!("Failed to serialize profile: {}", e))
            })?;
            profiles.push((name, json));
        }

        // Sort for stable ordering
        profiles.sort_by(|a, b| a.0.cmp(&b.0));

        Ok(profiles)
    }

    /// Hardcoded `ResolvedUserProfile::default()` serialized as JSON.
    ///
    /// The terminal fallthrough in the user-profile inheritance
    /// chain: per-TV user profile → named user profile → admin
    /// baseline → these compile-time defaults. Exposed so the
    /// GUI's Profiles pane can render the inherited-value column
    /// for every field without duplicating the constants
    /// client-side.
    ///
    /// Returned shape is a [`tisvlay_core::ResolvedUserProfile`] —
    /// fully resolved (no `Option<T>`); the GUI walks the chain
    /// itself using `ListUserProfiles` + `GetUserProfile` + this
    /// value.
    async fn get_resolved_profile_defaults(&self) -> Result<String, SessionError> {
        debug!("D-Bus: GetResolvedProfileDefaults");
        let defaults = tisvlay_core::ResolvedUserProfile::default();
        serde_json::to_string(&defaults).map_err(|e| {
            SessionError::OperationFailed(format!("Failed to serialize defaults: {}", e))
        })
    }

    /// Get the user profile name assigned to a TV.
    ///
    /// Returns "default" if no explicit assignment exists.
    async fn get_tv_user_profile(&self, tv_id: &str) -> String {
        debug!(tv_id = %tv_id, "D-Bus: GetTvUserProfile");
        let s = self.state.read().await;
        s.user_config.get_tv_user_profile_name(tv_id).to_string()
    }

    /// Assign a user profile to a TV.
    ///
    /// Saves to user config and re-resolves profiles immediately.
    /// Emits a `UserProfileChanged` signal on success.
    async fn set_tv_user_profile(
        &self,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        tv_id: &str,
        profile_name: &str,
    ) -> Result<(), SessionError> {
        info!(tv_id = %tv_id, profile = %profile_name, "D-Bus: SetTvUserProfile");

        let mut s = self.state.write().await;

        // Verify the profile exists (in user config, admin config, or as "default")
        if profile_name != "default"
            && !s.user_config.user.profiles.contains_key(profile_name)
            && !s.admin_user_profiles.contains_key(profile_name)
        {
            return Err(SessionError::ProfileNotFound(format!(
                "Profile '{}' not found in user or admin config",
                profile_name
            )));
        }

        // Update assignment
        s.user_config.set_tv_user_profile(tv_id, profile_name);

        // Re-resolve the user profile for this TV
        let resolved = s
            .user_config
            .resolve_user_profile(tv_id, &s.admin_user_default);
        s.profiles.insert(tv_id.to_string(), resolved);

        // Save user config to disk
        let save_result = match &self.config_path {
            Some(path) => s.user_config.save_to(path),
            None => s.user_config.save(),
        };
        if let Err(e) = save_result {
            warn!(error = %e, "Failed to save user config after profile change");
        }

        drop(s);

        // Notify subscribers that the profile changed
        if let Err(e) = Self::user_profile_changed(&ctxt, tv_id, profile_name).await {
            warn!(error = %e, tv_id = %tv_id, "Failed to emit UserProfileChanged signal");
        }

        Ok(())
    }

    /// Get a single user-profile body as JSON.
    ///
    /// Resolution order:
    ///
    /// 1. User config (unmerged — `null` fields preserved).
    /// 2. Admin baseline named profiles (admin's `[user.profiles.<name>]`
    ///    entries other than the merged-default one).
    /// 3. For the special name `"default"` only: the admin
    ///    user-default body (`admin_user_default`), which lives in
    ///    a separate field on `SessionState` because it has
    ///    semantic precedence over everything else.
    ///
    /// The GUI's Profiles pane editor reads this to render the
    /// inherited-value column for unset fields. Returns
    /// `ProfileNotFound` if the name is in none of those sources.
    async fn get_user_profile(&self, name: &str) -> Result<String, SessionError> {
        debug!(name, "D-Bus: GetUserProfile");
        let s = self.state.read().await;
        let profile = s
            .user_config
            .user
            .profiles
            .get(name)
            .or_else(|| s.admin_user_profiles.get(name))
            .cloned()
            .or_else(|| {
                if name == "default" {
                    Some(s.admin_user_default.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| SessionError::ProfileNotFound(name.to_string()))?;
        serde_json::to_string(&profile).map_err(|e| {
            SessionError::OperationFailed(format!("Failed to serialize profile: {}", e))
        })
    }

    /// Create a new user-profile entry under `name`.
    ///
    /// Fails with `ProfileExists` if the user config already has
    /// an entry with that name (admin presence doesn't count — the
    /// user is allowed to create overrides of admin profiles).
    /// On success, writes the user config to disk and emits
    /// `UserProfileListChanged(name, "created")`.
    async fn create_user_profile(
        &self,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        name: &str,
        json: &str,
    ) -> Result<(), SessionError> {
        info!(name, "D-Bus: CreateUserProfile");
        let profile: tisvlay_core::UserProfileConfig = serde_json::from_str(json)
            .map_err(|e| SessionError::OperationFailed(format!("Invalid profile JSON: {}", e)))?;
        {
            let mut s = self.state.write().await;
            if s.user_config.user.profiles.contains_key(name) {
                return Err(SessionError::ProfileExists(name.to_string()));
            }
            s.user_config
                .user
                .profiles
                .insert(name.to_string(), profile);
            self.save_user_config(&s)?;
        }
        Self::user_profile_list_changed(&ctxt, name, "created")
            .await
            .ok();
        Ok(())
    }

    /// Replace the body of an existing user-profile entry.
    ///
    /// Fails with `ProfileNotFound` if the user config doesn't
    /// have an entry with that name (use `CreateUserProfile` to
    /// add an override). On success, writes the user config to
    /// disk, re-resolves the cached `ResolvedUserProfile` for
    /// every TV that references this profile, and emits
    /// `UserProfileListChanged(name, "updated")`.
    async fn update_user_profile(
        &self,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        name: &str,
        json: &str,
    ) -> Result<(), SessionError> {
        info!(name, "D-Bus: UpdateUserProfile");
        let profile: tisvlay_core::UserProfileConfig = serde_json::from_str(json)
            .map_err(|e| SessionError::OperationFailed(format!("Invalid profile JSON: {}", e)))?;
        {
            let mut s = self.state.write().await;
            if !s.user_config.user.profiles.contains_key(name) {
                return Err(SessionError::ProfileNotFound(name.to_string()));
            }
            s.user_config
                .user
                .profiles
                .insert(name.to_string(), profile);
            self.save_user_config(&s)?;
            // Re-resolve every TV that references this profile so
            // dispatch logic sees the new body immediately.
            let tv_ids: Vec<String> = s
                .user_config
                .tv
                .iter()
                .filter(|tv| tv.profile.user == name)
                .map(|tv| tv.uuid.clone())
                .collect();
            for tv_id in tv_ids {
                let resolved = s
                    .user_config
                    .resolve_user_profile(&tv_id, &s.admin_user_default);
                s.profiles.insert(tv_id, resolved);
            }
        }
        Self::user_profile_list_changed(&ctxt, name, "updated")
            .await
            .ok();
        Ok(())
    }

    /// Delete a user-profile entry.
    ///
    /// Fails with `ProfileReserved` if `name == "default"` (the
    /// merged default is always visible; shadow individual fields
    /// via Update instead), `ProfileNotFound` if the user config
    /// doesn't have an entry with that name, or `ProfileInUse` if
    /// any `[[tv]]` entry in the user config references it (the
    /// error message names the referencing TV ids).
    async fn delete_user_profile(
        &self,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        name: &str,
    ) -> Result<(), SessionError> {
        info!(name, "D-Bus: DeleteUserProfile");
        if name == "default" {
            return Err(SessionError::ProfileReserved(name.to_string()));
        }
        {
            let mut s = self.state.write().await;
            if !s.user_config.user.profiles.contains_key(name) {
                return Err(SessionError::ProfileNotFound(name.to_string()));
            }
            let referencing: Vec<String> = s
                .user_config
                .tv
                .iter()
                .filter(|tv| tv.profile.user == name)
                .map(|tv| tv.uuid.clone())
                .collect();
            if !referencing.is_empty() {
                return Err(SessionError::ProfileInUse(format!(
                    "{}: {}",
                    name,
                    referencing.join(", ")
                )));
            }
            s.user_config.user.profiles.remove(name);
            self.save_user_config(&s)?;
        }
        Self::user_profile_list_changed(&ctxt, name, "deleted")
            .await
            .ok();
        Ok(())
    }

    /// Get the fully resolved user profile for a TV as JSON.
    ///
    /// Returns the user profile with all inheritance applied (user -> admin -> defaults).
    async fn get_effective_user_profile(&self, tv_id: &str) -> Result<String, SessionError> {
        debug!(tv_id = %tv_id, "D-Bus: GetEffectiveUserProfile");
        let s = self.state.read().await;

        let resolved = s.profiles.get(tv_id).cloned().unwrap_or_else(|| {
            s.user_config
                .resolve_user_profile(tv_id, &s.admin_user_default)
        });

        // Serialize ResolvedUserProfile as a UserProfileConfig-like JSON for consumers
        let json = serde_json::json!({
            "on_session_lock": resolved.on_session_lock,
            "on_session_unlock": resolved.on_session_unlock,
            "on_greeter_wake": resolved.on_greeter_wake,
            "idle_timeout_seconds": resolved.idle_timeout_seconds,
            "on_idle_timeout": resolved.on_idle_timeout,
            "on_idle_resume": resolved.on_idle_resume,
        });

        Ok(json.to_string())
    }

    // -------------------------------------------------------------------------
    // Signals
    // -------------------------------------------------------------------------

    /// Signal emitted when a TV's user profile assignment changes.
    ///
    /// Subscribers should refresh profile state for the affected TV.
    #[zbus(signal)]
    pub async fn user_profile_changed(
        emitter: &SignalEmitter<'_>,
        tv_id: &str,
        profile_name: &str,
    ) -> zbus::Result<()>;

    /// Signal emitted after a user profile is created, updated, or
    /// deleted via `CreateUserProfile` / `UpdateUserProfile` /
    /// `DeleteUserProfile`.
    ///
    /// `kind` is the lowercase verb (`"created"` / `"updated"` /
    /// `"deleted"`); subscribers (GUI Profiles pane) refetch the
    /// named profile on `created` / `updated` and remove it from
    /// their local list on `deleted`. Distinct from
    /// `UserProfileChanged(tv_id, name)` — that fires when a TV's
    /// *assignment* changes, this fires when the *catalog* changes.
    #[zbus(signal)]
    pub async fn user_profile_list_changed(
        emitter: &SignalEmitter<'_>,
        name: &str,
        kind: &str,
    ) -> zbus::Result<()>;

    /// Reload user configuration from disk.
    ///
    /// Also re-queries the daemon for the TV list and system default profile.
    async fn reload_config(&self) -> Result<(), SessionError> {
        info!("D-Bus: ReloadConfig");

        let new_state = SessionState::load(&self.daemon, self.config_path.as_ref())
            .await
            .map_err(|e| {
                SessionError::OperationFailed(format!("Failed to reload config: {}", e))
            })?;

        let mut s = self.state.write().await;
        *s = new_state;

        info!(tvs = s.tv_ids.len(), "Configuration reloaded via D-Bus");
        Ok(())
    }
}

/// Register the session D-Bus service on the session bus.
///
/// For production use. Tests should use [`register_session_dbus_on`]
/// with a [`BusKind::Address`] pointing at a private bus.
pub async fn register_session_dbus(
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<zbus::Connection> {
    register_session_dbus_on(tisvlay_core::BusKind::Session, state, daemon, config_path).await
}

/// Register the session D-Bus service on the given bus.
///
/// Accepts a [`BusKind`] so tests can point at a private
/// `dbus-daemon` via [`BusKind::Address`].
pub async fn register_session_dbus_on(
    bus: tisvlay_core::BusKind<'_>,
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<zbus::Connection> {
    let connection = bus
        .builder()
        .context("Failed to create session D-Bus builder")?
        .name(SESSION_DBUS_NAME)
        .context("Failed to request session D-Bus name")?
        .build()
        .await
        .context("Failed to build session D-Bus connection")?;

    let service = SessionService::new(state, daemon, config_path);
    connection
        .object_server()
        .at(SESSION_PATH, service)
        .await
        .context("Failed to register Session interface")?;

    info!("Session D-Bus interface registered at {}", SESSION_PATH);
    Ok(connection)
}
