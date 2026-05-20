//! D-Bus interface for the session service.
//!
//! Exposes `org.tisvlay.Session1` on the session bus for profile management.
//! Used by the tray app and future GUI to query and change profile assignments.

use crate::SessionState;
use anyhow::Context;
use std::sync::Arc;
use tisvlay_core::{DEFAULT_PROFILE_NAME, DaemonClient};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;
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
    /// Invalid argument supplied by the caller (e.g. malformed UUID,
    /// unknown profile UUID). Surfaces as
    /// `org.freedesktop.DBus.Error.InvalidArgs` on the wire so clients
    /// can route on the standard error name.
    #[zbus(name = "org.freedesktop.DBus.Error.InvalidArgs")]
    InvalidArgs(String),
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
        for profile in s.user_config.user.profiles.values() {
            merged.insert(profile.name.clone(), profile);
        }

        // Ensure "default" is always present
        if !merged.contains_key(DEFAULT_PROFILE_NAME) {
            merged.insert(DEFAULT_PROFILE_NAME.to_string(), &s.admin_user_default);
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

    /// Get the user profile UUID assigned to a TV (string-formatted).
    ///
    /// Returns [`DEFAULT_USER_PROFILE_UUID`] if no explicit assignment
    /// exists.
    ///
    /// [`DEFAULT_USER_PROFILE_UUID`]: tisvlay_core::DEFAULT_USER_PROFILE_UUID
    async fn get_tv_user_profile(&self, tv_id: &str) -> String {
        debug!(tv_id = %tv_id, "D-Bus: GetTvUserProfile");
        let s = self.state.read().await;
        s.user_config.get_tv_user_profile(tv_id).to_string()
    }

    /// Assign a user profile to a TV by UUID (string-formatted).
    ///
    /// Saves to user config and re-resolves profiles immediately.
    /// Emits a `UserProfileChanged` signal on success. Unknown UUIDs
    /// are rejected with `InvalidArgs`.
    async fn set_tv_user_profile(
        &self,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        tv_id: &str,
        profile_uuid: &str,
    ) -> Result<(), SessionError> {
        info!(tv_id = %tv_id, profile = %profile_uuid, "D-Bus: SetTvUserProfile");

        let parsed_uuid: Uuid = profile_uuid.parse().map_err(|_| {
            SessionError::InvalidArgs(format!("Invalid user profile UUID: {profile_uuid}"))
        })?;

        let mut s = self.state.write().await;

        // Accept the user-default UUID even when no `[user.profiles.<uuid>]`
        // body exists for it in the user config — resolution falls
        // through to the admin baseline in that case (same behavior
        // the `resolve_user_profile` chain already implements).
        // Otherwise the UUID must name an entry in the user catalog.
        let is_default = parsed_uuid == tisvlay_core::DEFAULT_USER_PROFILE_UUID;
        if !is_default && !s.user_config.user.profiles.contains_key(&parsed_uuid) {
            return Err(SessionError::InvalidArgs(format!(
                "Unknown user profile UUID: {profile_uuid}"
            )));
        }

        // Update assignment
        s.user_config.set_tv_user_profile(tv_id, parsed_uuid);

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
        if let Err(e) = Self::user_profile_changed(&ctxt, tv_id, profile_uuid).await {
            warn!(error = %e, tv_id = %tv_id, "Failed to emit UserProfileChanged signal");
        }

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
    /// `profile_uuid` is the UUID string of the newly assigned profile.
    /// Subscribers should refresh profile state for the affected TV.
    #[zbus(signal)]
    pub async fn user_profile_changed(
        emitter: &SignalEmitter<'_>,
        tv_id: &str,
        profile_uuid: &str,
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
