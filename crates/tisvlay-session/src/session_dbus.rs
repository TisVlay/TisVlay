//! D-Bus interface for the session service.
//!
//! Exposes `org.tisvlay.Session1` on the session bus for profile management.
//! Used by the tray app and future GUI to query and change profile assignments.

use crate::{
    SessionState, UserProfileError, create_user_profile, delete_user_profile,
    duplicate_user_profile, update_user_profile,
};
use anyhow::Context;
use std::collections::HashMap;
use std::sync::Arc;
use tisvlay_core::{DEFAULT_PROFILE_NAME, DaemonClient, UserProfileBody};
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
    /// Reserved profile UUID cannot be deleted / mutated this way
    /// (currently: delete of [`tisvlay_core::DEFAULT_USER_PROFILE_UUID`]).
    ReservedProfile(String),
    /// Profile body's `name` failed validation (empty, duplicate, or
    /// claimed the reserved `"default"` literal for a non-default
    /// UUID).
    ProfileNameInvalid(String),
    /// Delete rejected because at least one TV still references the
    /// profile via `[[tv]] profile.user`. Message text carries the
    /// TV count + UUID for diagnostics; routing is by error name.
    ProfileReferenced(String),
    /// Persistence failed after the in-memory mutation was rolled
    /// back. Maps from [`UserProfileError::Io`]; the underlying
    /// message is the `io::Error` Display.
    IoError(String),
}

impl From<UserProfileError> for SessionError {
    fn from(value: UserProfileError) -> Self {
        match value {
            UserProfileError::NotFound(uuid) => SessionError::ProfileNotFound(uuid),
            UserProfileError::Reserved(uuid) => SessionError::ReservedProfile(uuid),
            UserProfileError::NameInvalid(msg) => SessionError::ProfileNameInvalid(msg),
            UserProfileError::Referenced {
                profile_uuid,
                count,
            } => SessionError::ProfileReferenced(format!(
                "Profile {profile_uuid} still referenced by {count} TV(s)"
            )),
            UserProfileError::Io(msg) => SessionError::IoError(msg),
        }
    }
}

/// Parse a UUID coming off the user-profile CRUD wire surface.
/// Malformed UUIDs return `ProfileNotFound` rather than a generic
/// `InvalidArgs` so clients route "unknown profile" uniformly whether
/// the UUID is well-formed-but-absent or just garbage.
fn parse_profile_uuid(value: &str) -> Result<Uuid, SessionError> {
    Uuid::parse_str(value).map_err(|_| SessionError::ProfileNotFound(value.to_string()))
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
    /// the canonical XDG default (`~/.config/tisvlay/session.toml`)
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

    /// JSON-shape catalog: every user profile as `(name, json)` pairs
    /// where the JSON serializes a `UserProfileConfig`. Bound to the
    /// `ListUserProfilesJson` wire slot — the `ListUserProfiles` wire
    /// name carries the property-bag `a{sa{sv}}` shape.
    #[zbus(name = "ListUserProfilesJson")]
    async fn list_user_profiles(&self) -> Result<Vec<(String, String)>, SessionError> {
        debug!("D-Bus: ListUserProfilesJson");
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
    /// Emits a `TvUserProfileChanged` signal on success. Unknown UUIDs
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
        s.republish_idle_timeout();

        // Save user config to disk
        let save_result = match &self.config_path {
            Some(path) => s.user_config.save_to(path),
            None => s.user_config.save(),
        };
        if let Err(e) = save_result {
            warn!(error = %e, "Failed to save user config after profile change");
        }

        drop(s);

        // Notify subscribers that the profile assignment changed
        if let Err(e) = Self::tv_user_profile_changed(&ctxt, tv_id, profile_uuid).await {
            warn!(error = %e, tv_id = %tv_id, "Failed to emit TvUserProfileChanged signal");
        }

        Ok(())
    }

    /// Get the fully resolved user profile for a TV as JSON.
    ///
    /// Returns the user profile with all inheritance applied (user -> admin -> defaults).
    /// Bound to the `GetEffectiveUserProfileJson` wire slot — the
    /// `GetEffectiveUserProfile` wire name carries the property-bag
    /// `a{sv}` shape via `get_effective_user_profile_body`.
    #[zbus(name = "GetEffectiveUserProfileJson")]
    async fn get_effective_user_profile(&self, tv_id: &str) -> Result<String, SessionError> {
        debug!(tv_id = %tv_id, "D-Bus: GetEffectiveUserProfileJson");
        let s = self.state.read().await;

        let resolved = s.profiles.get(tv_id).cloned().unwrap_or_else(|| {
            s.user_config
                .resolve_user_profile(tv_id, &s.admin_user_default)
        });

        // Serialize ResolvedUserProfile as a UserProfileConfig-like JSON for consumers
        let json = serde_json::json!({
            "on_session_lock": resolved.on_session_lock,
            "on_session_unlock": resolved.on_session_unlock,
            "on_lock_wake": resolved.on_lock_wake,
            "idle_timeout_seconds": resolved.idle_timeout_seconds,
            "on_idle_timeout": resolved.on_idle_timeout,
            "on_idle_resume": resolved.on_idle_resume,
        });

        Ok(json.to_string())
    }

    // -------------------------------------------------------------------------
    // User profile property-bag CRUD
    //
    // Bodies round-trip as `UserProfileBody` (`a{sv}` via dict-derive);
    // the catalog list is `a{sa{sv}}` (uuid string → body dict). Mirror
    // of the `Manager1` system-profile CRUD shape, with the same eight
    // methods + three signals anchored on `Session1`.
    // -------------------------------------------------------------------------

    /// Catalog list: every user profile keyed by UUID string. Wire
    /// shape `a{sa{sv}}`. Includes the reserved-default UUID body
    /// (the admin baseline when no user-defined body shadows it).
    #[zbus(name = "ListUserProfiles")]
    async fn list_user_profile_bodies(
        &self,
    ) -> Result<HashMap<String, UserProfileBody>, SessionError> {
        debug!("D-Bus: ListUserProfiles");
        let s = self.state.read().await;
        Ok(s.list_user_profile_bodies()
            .into_iter()
            .map(|(uuid, body)| (uuid.to_string(), body))
            .collect())
    }

    /// Fetch one user-profile body by UUID. Returns
    /// `ProfileNotFound` if the UUID is malformed or absent from the
    /// catalog; the reserved-default UUID is always fetchable and
    /// surfaces the admin baseline body when no user body shadows it.
    async fn get_user_profile(&self, uuid: &str) -> Result<UserProfileBody, SessionError> {
        debug!(uuid = %uuid, "D-Bus: GetUserProfile");
        let parsed = parse_profile_uuid(uuid)?;
        let s = self.state.read().await;
        s.get_user_profile_body(parsed).map_err(Into::into)
    }

    /// Atomic full-body replace. Rename folds into this method — the
    /// new name lives on `body.name`.
    async fn update_user_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uuid: &str,
        body: UserProfileBody,
    ) -> Result<(), SessionError> {
        info!(uuid = %uuid, name = %body.name, "D-Bus: UpdateUserProfile");
        let parsed = parse_profile_uuid(uuid)?;
        update_user_profile(&self.state, self.config_path.as_ref(), parsed, body).await?;
        if let Err(e) = Self::user_profile_changed(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "Failed to emit UserProfileChanged");
        }
        Ok(())
    }

    /// Mint a new UUID, insert a minimal body under the given display
    /// name, persist, emit `UserProfileAdded(uuid)`.
    async fn create_user_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        name: &str,
    ) -> Result<String, SessionError> {
        info!(name = %name, "D-Bus: CreateUserProfile");
        let new_uuid = create_user_profile(&self.state, self.config_path.as_ref(), name).await?;
        let uuid_str = new_uuid.to_string();
        if let Err(e) = Self::user_profile_added(&emitter, &uuid_str).await {
            warn!(uuid = %uuid_str, error = %e, "Failed to emit UserProfileAdded");
        }
        Ok(uuid_str)
    }

    /// Clone an existing body under a new UUID + name, persist, emit
    /// `UserProfileAdded(new_uuid)`. The new body has every field
    /// from the source except `name`.
    async fn duplicate_user_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        source_uuid: &str,
        new_name: &str,
    ) -> Result<String, SessionError> {
        info!(source_uuid = %source_uuid, new_name = %new_name, "D-Bus: DuplicateUserProfile");
        let parsed = parse_profile_uuid(source_uuid)?;
        let new_uuid =
            duplicate_user_profile(&self.state, self.config_path.as_ref(), parsed, new_name)
                .await?;
        let uuid_str = new_uuid.to_string();
        if let Err(e) = Self::user_profile_added(&emitter, &uuid_str).await {
            warn!(uuid = %uuid_str, error = %e, "Failed to emit UserProfileAdded");
        }
        Ok(uuid_str)
    }

    /// Delete a profile. Reject the reserved default UUID, reject if
    /// at least one TV references the target via `[[tv]] profile.user`,
    /// persist, emit `UserProfileRemoved(uuid)`.
    async fn delete_user_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uuid: &str,
    ) -> Result<(), SessionError> {
        info!(uuid = %uuid, "D-Bus: DeleteUserProfile");
        let parsed = parse_profile_uuid(uuid)?;
        delete_user_profile(&self.state, self.config_path.as_ref(), parsed).await?;
        if let Err(e) = Self::user_profile_removed(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "Failed to emit UserProfileRemoved");
        }
        Ok(())
    }

    /// Hard-coded baseline: the body every user profile inherits from
    /// when its fields are all `None`. Static, but exposed so clients
    /// don't have to duplicate the table — `gettext("default")` is
    /// the client-side concern.
    fn get_resolved_user_profile_defaults(&self) -> UserProfileBody {
        debug!("D-Bus: GetResolvedUserProfileDefaults");
        SessionState::resolved_user_profile_defaults()
    }

    /// Per-TV resolved walk: returns the fully-resolved body for the
    /// named TV's assigned user profile (per-TV body → user default
    /// body → admin baseline → hardcoded defaults). Mirror of
    /// `Manager1.GetEffectiveSystemProfile`.
    async fn get_effective_user_profile_body(
        &self,
        tv_id: &str,
    ) -> Result<UserProfileBody, SessionError> {
        debug!(tv_id = %tv_id, "D-Bus: GetEffectiveUserProfile");
        let s = self.state.read().await;
        Ok(s.get_effective_user_profile_body(tv_id))
    }

    // -------------------------------------------------------------------------
    // Signals
    // -------------------------------------------------------------------------

    /// Signal emitted when a TV's user profile assignment changes
    /// via `SetTvUserProfile`. `profile_uuid` is the UUID string of
    /// the newly assigned profile; subscribers should refresh profile
    /// state for the affected TV.
    #[zbus(signal)]
    pub async fn tv_user_profile_changed(
        emitter: &SignalEmitter<'_>,
        tv_id: &str,
        profile_uuid: &str,
    ) -> zbus::Result<()>;

    /// A user profile's body was mutated (any field, including
    /// `name` — rename folds into `UpdateUserProfile`).
    ///
    /// Clients with a cached body for this UUID should re-fetch via
    /// `GetUserProfile(uuid)`. Fires once per successful
    /// `UpdateUserProfile` after persistence; clients are guaranteed
    /// to be able to fetch the new body the moment the signal arrives.
    #[zbus(signal)]
    pub async fn user_profile_changed(emitter: &SignalEmitter<'_>, uuid: &str) -> zbus::Result<()>;

    /// A new user profile appeared in the catalog (Create or
    /// Duplicate). Body is fetchable by the moment the signal
    /// arrives — both flows commit body + emit under the same write
    /// lock.
    #[zbus(signal)]
    pub async fn user_profile_added(emitter: &SignalEmitter<'_>, uuid: &str) -> zbus::Result<()>;

    /// A user profile was removed from the catalog. Clients should
    /// drop any cached body for this UUID.
    #[zbus(signal)]
    pub async fn user_profile_removed(emitter: &SignalEmitter<'_>, uuid: &str) -> zbus::Result<()>;

    /// Reload user configuration from disk.
    ///
    /// Also re-queries the daemon for the TV list and system default profile.
    ///
    /// Emits per-profile `UserProfileAdded` / `UserProfileChanged` /
    /// `UserProfileRemoved` signals for any catalog entries that
    /// appeared, mutated, or disappeared during the reload, so a direct
    /// user-config-file edit is observationally identical to an
    /// external mutation call. Without these, clients with cached
    /// profile bodies stay stale until they happen to call a `Get*`
    /// method. Failures are best-effort.
    async fn reload_config(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), SessionError> {
        info!("D-Bus: ReloadConfig");

        let new_state = SessionState::load(&self.daemon, self.config_path.as_ref())
            .await
            .map_err(|e| {
                SessionError::OperationFailed(format!("Failed to reload config: {}", e))
            })?;

        // Snapshot the old user-profile catalog under the write lock,
        // swap, then diff against the new catalog. UUID ordering
        // follows the BTreeMap (sorted) so the signal sequence is
        // deterministic across reloads.
        let (added, changed, removed) = {
            let mut s = self.state.write().await;
            let old_profiles = std::mem::take(&mut s.user_config.user.profiles);
            *s = new_state;
            let new_profiles = &s.user_config.user.profiles;

            let mut added = Vec::new();
            let mut changed = Vec::new();
            let mut removed = Vec::new();
            for (uuid, new_body) in new_profiles {
                match old_profiles.get(uuid) {
                    Some(old_body) if old_body == new_body => { /* unchanged */ }
                    Some(_) => changed.push(uuid.to_string()),
                    None => added.push(uuid.to_string()),
                }
            }
            for uuid in old_profiles.keys() {
                if !new_profiles.contains_key(uuid) {
                    removed.push(uuid.to_string());
                }
            }
            info!(tvs = s.tv_ids.len(), "Configuration reloaded via D-Bus");
            (added, changed, removed)
        };

        for uuid in &added {
            if let Err(e) = Self::user_profile_added(&emitter, uuid).await {
                warn!(uuid = %uuid, error = %e, "ReloadConfig: failed to emit UserProfileAdded");
            }
        }
        for uuid in &changed {
            if let Err(e) = Self::user_profile_changed(&emitter, uuid).await {
                warn!(
                    uuid = %uuid,
                    error = %e,
                    "ReloadConfig: failed to emit UserProfileChanged"
                );
            }
        }
        for uuid in &removed {
            if let Err(e) = Self::user_profile_removed(&emitter, uuid).await {
                warn!(uuid = %uuid, error = %e, "ReloadConfig: failed to emit UserProfileRemoved");
            }
        }

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
