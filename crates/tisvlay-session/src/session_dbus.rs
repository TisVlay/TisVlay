//! D-Bus interface for the session service.
//!
//! Exposes `org.tisvlay.Session1` on the session bus for profile management.
//! Used by the tray app and future GUI to query and change profile assignments.

use crate::SessionState;
use std::sync::Arc;
use tisvlay_core::DaemonClient;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use zbus::interface;

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
}

/// D-Bus service for session profile management.
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
    /// List all available profiles.
    ///
    /// Returns: Array of (profile_name, profile_json) tuples.
    /// Includes user-defined profiles merged with system profiles.
    /// User profiles take precedence over system profiles with the same name.
    async fn list_profiles(&self) -> Result<Vec<(String, String)>, SessionError> {
        debug!("D-Bus: ListProfiles");
        let s = self.state.read().await;

        // Start with system profiles as the base
        let mut merged: std::collections::HashMap<String, &tisvlay_core::ProfileConfig> =
            std::collections::HashMap::new();
        for (name, profile) in &s.system_profiles {
            merged.insert(name.clone(), profile);
        }

        // User profiles override system profiles with the same name
        for (name, profile) in &s.user_config.profiles {
            merged.insert(name.clone(), profile);
        }

        // Ensure "default" is always present
        if !merged.contains_key("default") {
            merged.insert("default".to_string(), &s.system_default);
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

    /// Get the profile name assigned to a TV.
    ///
    /// Returns "default" if no explicit assignment exists.
    async fn get_tv_profile(&self, tv_id: &str) -> String {
        debug!(tv_id = %tv_id, "D-Bus: GetTvProfile");
        let s = self.state.read().await;
        s.user_config.get_tv_profile_name(tv_id).to_string()
    }

    /// Assign a profile to a TV.
    ///
    /// Saves to user config and re-resolves profiles immediately.
    async fn set_tv_profile(
        &self,
        tv_id: &str,
        profile_name: &str,
    ) -> Result<(), SessionError> {
        info!(tv_id = %tv_id, profile = %profile_name, "D-Bus: SetTvProfile");

        let mut s = self.state.write().await;

        // Verify the profile exists (in user config, system config, or as "default")
        if profile_name != "default"
            && !s.user_config.profiles.contains_key(profile_name)
            && !s.system_profiles.contains_key(profile_name)
        {
            return Err(SessionError::ProfileNotFound(format!(
                "Profile '{}' not found in user or system config",
                profile_name
            )));
        }

        // Update assignment
        s.user_config.set_tv_profile(tv_id, profile_name);

        // Re-resolve the profile for this TV
        let resolved = s
            .user_config
            .resolve_profile(tv_id, &s.system_default);
        s.profiles.insert(tv_id.to_string(), resolved);

        // Save user config to disk
        let save_result = match &self.config_path {
            Some(path) => s.user_config.save_to(path),
            None => s.user_config.save(),
        };
        if let Err(e) = save_result {
            warn!(error = %e, "Failed to save user config after profile change");
        }

        Ok(())
    }

    /// Get the fully resolved profile for a TV as JSON.
    ///
    /// Returns the profile with all inheritance applied (user -> system -> defaults).
    async fn get_effective_profile(&self, tv_id: &str) -> Result<String, SessionError> {
        debug!(tv_id = %tv_id, "D-Bus: GetEffectiveProfile");
        let s = self.state.read().await;

        let resolved = s
            .profiles
            .get(tv_id)
            .cloned()
            .unwrap_or_else(|| s.user_config.resolve_profile(tv_id, &s.system_default));

        // Serialize ResolvedProfile as a ProfileConfig-like JSON for consumers
        let json = serde_json::json!({
            "on_screen_lock": resolved.on_screen_lock,
            "on_screen_unlock": resolved.on_screen_unlock,
            "on_greeter_wake": resolved.on_greeter_wake,
            "on_sleep": resolved.on_sleep,
            "on_wake": resolved.on_wake,
            "idle_timeout_seconds": resolved.idle_timeout_seconds,
            "on_idle_timeout": resolved.on_idle_timeout,
        });

        Ok(json.to_string())
    }

    /// Reload user configuration from disk.
    ///
    /// Also re-queries the daemon for the TV list and system default profile.
    async fn reload_config(&self) -> Result<(), SessionError> {
        info!("D-Bus: ReloadConfig");

        let new_state =
            SessionState::load(&self.daemon, self.config_path.as_ref())
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

/// Register the session D-Bus service.
pub async fn register_session_dbus(
    state: Arc<RwLock<SessionState>>,
    daemon: Arc<DaemonClient>,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<zbus::Connection> {
    let connection = zbus::connection::Builder::session()
        .context("Failed to connect to session D-Bus")?
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

use anyhow::Context;
