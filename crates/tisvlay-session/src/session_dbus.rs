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

        // The desktop mapping rebuilt from the new profile; pushed to
        // the daemon after the write lock is released so the Manager1
        // call can't deadlock against our own state lock.
        let mapping = {
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
            s.profiles.insert(tv_id.to_string(), resolved.clone());

            // Save user config to disk
            let save_result = match &self.config_path {
                Some(path) => s.user_config.save_to(path),
                None => s.user_config.save(),
            };
            if let Err(e) = save_result {
                warn!(error = %e, "Failed to save user config after profile change");
            }

            // Build the fresh DesktopMapping so the daemon republishes
            // the new sync modes on Tv1.Desktop and the state forwarder
            // can gate the kernel-backlight device accordingly.
            let identity = s
                .user_config
                .tv
                .iter()
                .find(|e| e.id == tv_id)
                .and_then(|e| e.desktop.as_ref())
                .cloned();
            tisvlay_core::DesktopMapping::from_parts(
                identity.as_ref(),
                resolved.sync_backlight,
                resolved.sync_volume,
            )
        };

        // Push the updated mapping to the daemon so `Tv1.Desktop.*`
        // re-reads and `state_forwarder` re-evaluates brightness
        // lifecycle under the new profile's sync modes.
        if let Err(e) = self.daemon.set_desktop_mapping(tv_id, mapping).await {
            warn!(error = %e, tv_id = %tv_id, "Failed to push updated desktop mapping to daemon");
        }

        // Notify subscribers that the profile changed
        if let Err(e) = Self::user_profile_changed(&ctxt, tv_id, profile_name).await {
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
            "sync_backlight": resolved.sync_backlight,
            "sync_volume": resolved.sync_volume,
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
