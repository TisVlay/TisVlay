//! Per-profile D-Bus interface for user profiles.
//!
//! One `UserProfile1` object lives at `/org/tisvlay/UserProfile/<UUID_>`
//! per user profile in the session-owned catalog. Each typed field
//! property reports the **resolved** value after walking the
//! inheritance chain (this profile → user-config default
//! → admin-baseline profile-by-name → admin-baseline default →
//! hardcoded baseline). `LocallySetFields` names the fields the user
//! has explicitly set versus inheriting. Editing rides on standard
//! `Properties.Set` + `Properties.PropertiesChanged` rather than
//! custom JSON RPCs — mirrors the daemon-side `SystemProfile1`
//! pattern.
//!
//! Cached fields are populated at registration and kept fresh by the
//! [`user_profile_state_forwarder`] task — same mechanic the daemon's
//! `state_forwarder` uses for the `SystemProfileBodyChanged` event.
//! Mutation helpers emit a [`UserProfileEvent::BodyChanged`] through
//! the shared channel and the forwarder runs the per-property refresh
//! out across every registered `UserProfile1` interface (default-UUID
//! edits fan out across the whole catalog).
//!
//! `IdleTimeoutSeconds` carries `0` as a *legitimate* value (idle
//! timer disabled); the disambiguation between "set to 0" and
//! "unset / inheriting" rides on `LocallySetFields`, which lists
//! `idle_timeout_seconds` only when the underlying body field is
//! `Some(_)`.

use crate::SessionState;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::dbus_paths::user_profile_path;
use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SessionAction, UserProfileConfig,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info};
use uuid::Uuid;
use zbus::interface;

/// Event emitted when a user-profile body or per-TV reference set
/// changes. Routed through the session-side event channel to the
/// [`user_profile_state_forwarder`] task, which refreshes cached
/// D-Bus properties on every affected `UserProfile1` interface and
/// emits `PropertiesChanged`.
#[derive(Debug, Clone)]
pub enum UserProfileEvent {
    /// A user-profile body was mutated in place — typed
    /// `Properties.Set` / `ResetField` / `ResetAll` on
    /// `UserProfile1`, or a name rewrite cascading into the
    /// `[[tv]] profile.user` references.
    ///
    /// When `uuid == DEFAULT_USER_PROFILE_UUID` every other profile
    /// inherits from the default chain, so the forwarder fans out the
    /// refresh across the whole catalog.
    BodyChanged { uuid: Uuid },
    /// A TV's `profile.user` reassignment moved its membership from
    /// one profile to another. Forwarder refreshes `ReferencedBy` on
    /// every UUID in the set (previously-affected + newly-affected).
    TvReassigned { affected_uuids: HashSet<Uuid> },
}

/// Sender side of the user-profile event channel.
pub type UserProfileEventTx = mpsc::UnboundedSender<UserProfileEvent>;

/// Cached state for one `UserProfile1` D-Bus object.
///
/// Mirrors the daemon-side `SystemProfileService`: typed string fields
/// back the `#[zbus(property)]` getters, `&mut self` setters mutate
/// state via the session-state helpers and update the cache so zbus's
/// macro emits `PropertiesChanged` automatically for the
/// being-written property. `LocallySetFields` and cross-profile
/// `_changed` signals ride through the forwarder task.
///
/// `idle_timeout_seconds` rides as a `u32` on the wire (D-Bus `u`);
/// the body stores `u64` but the cached field is downcast at
/// registration / refresh time.
#[derive(Debug)]
pub struct UserProfileService {
    pub uuid: Uuid,
    pub state: Arc<RwLock<SessionState>>,
    pub config_path: Option<PathBuf>,
    pub events: UserProfileEventTx,
    pub name: String,
    pub on_session_lock: String,
    pub on_session_unlock: String,
    pub on_greeter_wake: String,
    pub on_idle_timeout: String,
    pub on_idle_resume: String,
    pub idle_timeout_seconds: u32,
    pub locally_set_fields: Vec<String>,
    pub referenced_by: Vec<String>,
}

#[interface(name = "org.tisvlay.UserProfile1")]
impl UserProfileService {
    /// Profile UUID. Const for the lifetime of the object — the
    /// session re-registers at the same path on `ReloadConfig`, so
    /// `Id` is effectively immutable from the client's perspective.
    #[zbus(property(emits_changed_signal = "const"))]
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    /// Friendly profile name.
    #[zbus(property)]
    fn name(&self) -> &str {
        &self.name
    }

    /// Set the friendly profile name.
    ///
    /// Validates non-empty + uniqueness + reserved-name rules. The
    /// default-UUID profile is locked to `"default"`; non-default
    /// profiles cannot be renamed to `"default"`.
    #[zbus(property)]
    async fn set_name(&mut self, name: String) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, name = %name, "D-Bus: UserProfile1.set Name");
        set_user_profile_field(
            &self.state,
            self.config_path.as_ref(),
            self.uuid,
            "name",
            &name,
        )
        .await
        .map_err(profile_err)?;
        self.name = name;
        // Forwarder runs the cross-profile refresh (referenced-by on
        // every profile whose TV-assignment set shifted via the
        // cascaded rename, plus LocallySetFields recompute).
        let _ = self
            .events
            .send(UserProfileEvent::BodyChanged { uuid: self.uuid });
        Ok(())
    }

    /// Resolved `on_session_lock` action.
    #[zbus(property)]
    fn on_session_lock(&self) -> &str {
        &self.on_session_lock
    }

    /// Set `on_session_lock`. Promotes the field to locally-set even
    /// if the value matches the inherited fallthrough — explicit
    /// intent is preserved across edits. Use `ResetField` to go back
    /// to inheriting.
    #[zbus(property)]
    async fn set_on_session_lock(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_session_lock", value).await
    }

    /// Resolved `on_session_unlock` action.
    #[zbus(property)]
    fn on_session_unlock(&self) -> &str {
        &self.on_session_unlock
    }

    #[zbus(property)]
    async fn set_on_session_unlock(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_session_unlock", value).await
    }

    /// Resolved `on_greeter_wake` action.
    #[zbus(property)]
    fn on_greeter_wake(&self) -> &str {
        &self.on_greeter_wake
    }

    #[zbus(property)]
    async fn set_on_greeter_wake(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_greeter_wake", value).await
    }

    /// Resolved `on_idle_timeout` action.
    #[zbus(property)]
    fn on_idle_timeout(&self) -> &str {
        &self.on_idle_timeout
    }

    #[zbus(property)]
    async fn set_on_idle_timeout(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_idle_timeout", value).await
    }

    /// Resolved `on_idle_resume` action.
    #[zbus(property)]
    fn on_idle_resume(&self) -> &str {
        &self.on_idle_resume
    }

    #[zbus(property)]
    async fn set_on_idle_resume(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_idle_resume", value).await
    }

    /// Resolved idle-timeout (seconds). `0` is a *legitimate* value
    /// meaning "idle timer disabled"; the unset-vs-zero distinction
    /// rides on `LocallySetFields`.
    #[zbus(property)]
    fn idle_timeout_seconds(&self) -> u32 {
        self.idle_timeout_seconds
    }

    /// Set the idle-timeout. Accepts any non-negative value
    /// including `0` — `0` flags the timer as explicitly disabled
    /// (still recorded in `LocallySetFields`, so the GUI can
    /// distinguish "user set 0" from "inherits 0 from baseline").
    #[zbus(property)]
    async fn set_idle_timeout_seconds(&mut self, value: u32) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, value, "D-Bus: UserProfile1.set IdleTimeoutSeconds");
        set_user_profile_idle_timeout_seconds(
            &self.state,
            self.config_path.as_ref(),
            self.uuid,
            value as u64,
        )
        .await
        .map_err(profile_err)?;
        self.idle_timeout_seconds = value;
        let _ = self
            .events
            .send(UserProfileEvent::BodyChanged { uuid: self.uuid });
        Ok(())
    }

    /// Names of the fields this profile explicitly sets. Drives the
    /// GUI's gutter accent + `(inherit: X)` placeholder visibility.
    #[zbus(property)]
    fn locally_set_fields(&self) -> Vec<String> {
        self.locally_set_fields.clone()
    }

    /// UUIDs of TVs that reference this profile via the user-config
    /// `[[tv]] profile.user = "<name>"` assignment. Resolved name →
    /// UUID at read time so renames don't shift the reference.
    #[zbus(property)]
    fn referenced_by(&self) -> Vec<String> {
        self.referenced_by.clone()
    }

    /// Clear one field back to inheriting. Triggers
    /// `PropertiesChanged` on both the field's property (resolved
    /// value may shift) and `LocallySetFields` via the forwarder.
    async fn reset_field(&self, field: &str) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, field = %field, "D-Bus: UserProfile1.ResetField");
        reset_user_profile_field(&self.state, self.config_path.as_ref(), self.uuid, field)
            .await
            .map_err(profile_err)?;
        let _ = self
            .events
            .send(UserProfileEvent::BodyChanged { uuid: self.uuid });
        Ok(())
    }

    /// Clear every field back to inheriting. One round-trip; the
    /// forwarder fires `PropertiesChanged` for every property whose
    /// cached value shifts.
    async fn reset_all(&self) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, "D-Bus: UserProfile1.ResetAll");
        reset_user_profile(&self.state, self.config_path.as_ref(), self.uuid)
            .await
            .map_err(profile_err)?;
        let _ = self
            .events
            .send(UserProfileEvent::BodyChanged { uuid: self.uuid });
        Ok(())
    }
}

impl UserProfileService {
    /// Helper for the five typed-action setters that follow the
    /// same shape — parse + persist via session-state helper, update
    /// the cached field on success. zbus's `#[interface]` macro
    /// generates a `<field>_changed()` emit when a `&mut self`
    /// setter mutates state; the cross-profile fan-out (default-edit
    /// shifting every other profile's resolved value) is handled in
    /// the forwarder.
    async fn set_field(&mut self, field: &str, value: String) -> zbus::fdo::Result<()> {
        info!(
            uuid = %self.uuid,
            field = %field,
            value = %value,
            "D-Bus: UserProfile1.set field"
        );
        set_user_profile_field(
            &self.state,
            self.config_path.as_ref(),
            self.uuid,
            field,
            &value,
        )
        .await
        .map_err(profile_err)?;
        match field {
            "on_session_lock" => self.on_session_lock = value,
            "on_session_unlock" => self.on_session_unlock = value,
            "on_greeter_wake" => self.on_greeter_wake = value,
            "on_idle_timeout" => self.on_idle_timeout = value,
            "on_idle_resume" => self.on_idle_resume = value,
            _ => {}
        }
        let _ = self
            .events
            .send(UserProfileEvent::BodyChanged { uuid: self.uuid });
        Ok(())
    }
}

/// Map a [`UserProfileMutationError`] to a `zbus::fdo` error. Parse /
/// range failures surface as `InvalidArgs` so clients can route on
/// the error name without parsing the body string. Reserved + in-use
/// failures route as `AccessDenied` so delete-rejection error names
/// are distinguishable from validation rejections without parsing
/// the message.
pub(crate) fn profile_err(e: UserProfileMutationError) -> zbus::fdo::Error {
    match e {
        UserProfileMutationError::Invalid(msg) => zbus::fdo::Error::InvalidArgs(msg),
        UserProfileMutationError::Failed(msg) => zbus::fdo::Error::Failed(msg),
        UserProfileMutationError::Reserved(msg) => zbus::fdo::Error::AccessDenied(msg),
        UserProfileMutationError::InUse { name, tvs } => zbus::fdo::Error::AccessDenied(format!(
            "profile '{}' is referenced by TVs: {}",
            name,
            tvs.join(", ")
        )),
    }
}

/// Errors from user-profile mutation helpers. Distinguishes typed-
/// argument failures (bad enum value, reserved-name violation) from
/// state-machine failures (unknown UUID, disk I/O).
#[derive(Debug)]
pub enum UserProfileMutationError {
    /// Caller-supplied input was invalid (enum parse, reserved name,
    /// non-empty name, duplicate name). Maps to `InvalidArgs`.
    Invalid(String),
    /// Operation failed for state-machine reasons (unknown UUID,
    /// persistence I/O error). Maps to `Failed`.
    Failed(String),
    /// Operation targets a reserved profile (e.g. trying to delete
    /// the `"default"` profile). Maps to
    /// `org.freedesktop.DBus.Error.AccessDenied`.
    Reserved(String),
    /// Profile delete blocked because TVs still reference it. `name`
    /// is the friendly name (log/UI context); `tvs` is the list of
    /// offending TV UUIDs. Maps to
    /// `org.freedesktop.DBus.Error.AccessDenied`.
    InUse { name: String, tvs: Vec<String> },
}

impl std::fmt::Display for UserProfileMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Failed(m) | Self::Reserved(m) => f.write_str(m),
            Self::InUse { name, tvs } => write!(
                f,
                "profile '{}' is referenced by TVs: {}",
                name,
                tvs.join(", ")
            ),
        }
    }
}

impl std::error::Error for UserProfileMutationError {}

/// Parse a [`SessionAction`] from its serde `rename_all =
/// "snake_case"` wire form. Surfaces failure as
/// [`UserProfileMutationError::Invalid`].
fn parse_session_action(value: &str) -> Result<SessionAction, UserProfileMutationError> {
    match value {
        "none" => Ok(SessionAction::None),
        "screen_off" => Ok(SessionAction::ScreenOff),
        "screen_on" => Ok(SessionAction::ScreenOn),
        "power_off" => Ok(SessionAction::PowerOff),
        "power_on" => Ok(SessionAction::PowerOn),
        _ => Err(UserProfileMutationError::Invalid(format!(
            "Invalid SessionAction value: {}",
            value
        ))),
    }
}

/// Whether `field` is a writable typed action property on a
/// `UserProfile1` interface (excluding `name` and
/// `idle_timeout_seconds`, which have bespoke parsing).
fn is_known_user_profile_action_field(field: &str) -> bool {
    matches!(
        field,
        "on_session_lock"
            | "on_session_unlock"
            | "on_greeter_wake"
            | "on_idle_timeout"
            | "on_idle_resume"
    )
}

/// Apply one parsed-and-validated typed-action field to a
/// `UserProfileConfig` body. `field` must have already passed
/// [`is_known_user_profile_action_field`] (or be `"name"`).
fn apply_user_profile_action_field(
    body: &mut UserProfileConfig,
    field: &str,
    value: &str,
) -> Result<(), UserProfileMutationError> {
    match field {
        "name" => body.name = value.to_string(),
        "on_session_lock" => body.on_session_lock = Some(parse_session_action(value)?),
        "on_session_unlock" => body.on_session_unlock = Some(parse_session_action(value)?),
        "on_greeter_wake" => body.on_greeter_wake = Some(parse_session_action(value)?),
        "on_idle_timeout" => body.on_idle_timeout = Some(parse_session_action(value)?),
        "on_idle_resume" => body.on_idle_resume = Some(parse_session_action(value)?),
        _ => {
            return Err(UserProfileMutationError::Invalid(format!(
                "Unknown user profile field: {}",
                field
            )));
        }
    }
    Ok(())
}

/// Clear one field on a `UserProfileConfig` body back to `None`.
fn clear_user_profile_field(body: &mut UserProfileConfig, field: &str) {
    match field {
        "on_session_lock" => body.on_session_lock = None,
        "on_session_unlock" => body.on_session_unlock = None,
        "on_greeter_wake" => body.on_greeter_wake = None,
        "on_idle_timeout" => body.on_idle_timeout = None,
        "on_idle_resume" => body.on_idle_resume = None,
        "idle_timeout_seconds" => body.idle_timeout_seconds = None,
        _ => {}
    }
}

/// Mutate one field on a user profile body.
///
/// Parses `value` against the field's typed enum (`SessionAction`),
/// stores it as `Some(_)` on the body, persists the user config, and
/// re-resolves the per-TV cached profiles.
///
/// Field `"name"` validates non-empty + uniqueness; the reserved
/// [`DEFAULT_PROFILE_NAME`] is rejected as a target for any
/// non-default profile, and the default-UUID profile is locked to
/// the reserved name.
pub async fn set_user_profile_field(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
    field: &str,
    value: &str,
) -> Result<(), UserProfileMutationError> {
    // Validate before touching state. Name has bespoke rules; typed
    // enum fields parse against their serde representation.
    if field == "name" {
        if value.is_empty() {
            return Err(UserProfileMutationError::Invalid(
                "Profile name must not be empty".into(),
            ));
        }
        if uuid == DEFAULT_USER_PROFILE_UUID && value != DEFAULT_PROFILE_NAME {
            return Err(UserProfileMutationError::Invalid(format!(
                "Default user profile cannot be renamed (must stay '{}')",
                DEFAULT_PROFILE_NAME
            )));
        }
        if uuid != DEFAULT_USER_PROFILE_UUID && value == DEFAULT_PROFILE_NAME {
            return Err(UserProfileMutationError::Invalid(format!(
                "'{}' is reserved for the default user profile",
                DEFAULT_PROFILE_NAME
            )));
        }
        let s = state.read().await;
        if let Some((existing_uuid, _)) = s.user_config.user.find_profile_by_name(value)
            && existing_uuid != uuid
        {
            return Err(UserProfileMutationError::Invalid(format!(
                "A user profile named '{}' already exists",
                value
            )));
        }
        drop(s);
    } else if field == "idle_timeout_seconds" {
        // Numeric path — callers should use the dedicated helper
        // rather than routing through the string setter.
        return Err(UserProfileMutationError::Invalid(
            "Use SetIdleTimeoutSeconds for the numeric field".into(),
        ));
    } else if !is_known_user_profile_action_field(field) {
        return Err(UserProfileMutationError::Invalid(format!(
            "Unknown user profile field: {}",
            field
        )));
    } else {
        // All other writable fields are SessionAction-typed.
        parse_session_action(value)?;
    }

    // Mutate in-memory state. References are UUID-keyed end-to-end,
    // so a rename is just a body-field edit; no reference cascade.
    {
        let mut s = state.write().await;
        let body = s.user_config.user.profiles.get_mut(&uuid).ok_or_else(|| {
            UserProfileMutationError::Failed(format!("Unknown user profile uuid: {}", uuid))
        })?;
        apply_user_profile_action_field(body, field, value)?;
    }

    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;

    debug!(uuid = %uuid, field = %field, "User profile field updated");
    Ok(())
}

/// Numeric variant of [`set_user_profile_field`] for
/// `idle_timeout_seconds`. Stores as `Some(value)` on the body; `0`
/// is accepted as a legitimate "disable idle timer" value (the
/// "set 0 vs unset" disambiguation rides on `LocallySetFields`, not
/// on a sentinel).
pub async fn set_user_profile_idle_timeout_seconds(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
    value: u64,
) -> Result<(), UserProfileMutationError> {
    {
        let mut s = state.write().await;
        let body = s.user_config.user.profiles.get_mut(&uuid).ok_or_else(|| {
            UserProfileMutationError::Failed(format!("Unknown user profile uuid: {}", uuid))
        })?;
        body.idle_timeout_seconds = Some(value);
    }
    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;
    debug!(uuid = %uuid, value, "User profile idle_timeout_seconds updated");
    Ok(())
}

/// Clear one field on a user profile back to `None` (inheriting).
pub async fn reset_user_profile_field(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
    field: &str,
) -> Result<(), UserProfileMutationError> {
    if field == "name" {
        return Err(UserProfileMutationError::Invalid(
            "'name' cannot be reset — names are always explicit".into(),
        ));
    }
    if !is_known_user_profile_action_field(field) && field != "idle_timeout_seconds" {
        return Err(UserProfileMutationError::Invalid(format!(
            "Unknown user profile field: {}",
            field
        )));
    }
    {
        let mut s = state.write().await;
        let body = s.user_config.user.profiles.get_mut(&uuid).ok_or_else(|| {
            UserProfileMutationError::Failed(format!("Unknown user profile uuid: {}", uuid))
        })?;
        clear_user_profile_field(body, field);
    }
    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;
    debug!(uuid = %uuid, field = %field, "User profile field reset");
    Ok(())
}

/// Create a new user profile with the given friendly name and every
/// field inheriting. Generates a fresh [`Uuid::new_v4`], validates the
/// name (non-empty, not reserved, not a duplicate), inserts the body,
/// persists, and returns the new UUID.
///
/// The D-Bus handler registers the per-profile `UserProfile1`
/// interface against this UUID immediately after this call returns so
/// `ObjectManager.InterfacesAdded` fires on the user bus.
pub async fn create_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    name: String,
) -> Result<Uuid, UserProfileMutationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(UserProfileMutationError::Invalid(
            "profile name cannot be empty".into(),
        ));
    }
    if trimmed == DEFAULT_PROFILE_NAME {
        return Err(UserProfileMutationError::Invalid(format!(
            "'{}' is reserved for the default user profile",
            DEFAULT_PROFILE_NAME
        )));
    }
    {
        let s = state.read().await;
        if s.user_config.user.find_profile_by_name(trimmed).is_some() {
            return Err(UserProfileMutationError::Invalid(format!(
                "A user profile named '{}' already exists",
                trimmed
            )));
        }
    }

    let uuid = Uuid::new_v4();
    {
        let mut s = state.write().await;
        s.user_config.user.profiles.insert(
            uuid,
            UserProfileConfig {
                name: trimmed.to_string(),
                ..Default::default()
            },
        );
    }

    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;
    info!(uuid = %uuid, name = %trimmed, "User profile created");
    Ok(uuid)
}

/// Duplicate an existing user profile under a new friendly name. The
/// new profile inherits the source's locally-set fields verbatim
/// (inheriting fields stay inheriting), gets a fresh UUID, and has an
/// empty `ReferencedBy` set.
pub async fn duplicate_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    source_uuid: Uuid,
    new_name: String,
) -> Result<Uuid, UserProfileMutationError> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err(UserProfileMutationError::Invalid(
            "profile name cannot be empty".into(),
        ));
    }
    if trimmed == DEFAULT_PROFILE_NAME {
        return Err(UserProfileMutationError::Invalid(format!(
            "'{}' is reserved for the default user profile",
            DEFAULT_PROFILE_NAME
        )));
    }

    let new_uuid = Uuid::new_v4();
    {
        let mut s = state.write().await;
        if s.user_config.user.find_profile_by_name(trimmed).is_some() {
            return Err(UserProfileMutationError::Invalid(format!(
                "A user profile named '{}' already exists",
                trimmed
            )));
        }
        let Some(source_body) = s.user_config.user.profiles.get(&source_uuid).cloned() else {
            return Err(UserProfileMutationError::Invalid(format!(
                "Unknown user profile uuid: {}",
                source_uuid
            )));
        };
        let mut new_body = source_body;
        new_body.name = trimmed.to_string();
        s.user_config.user.profiles.insert(new_uuid, new_body);
    }

    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;
    info!(source = %source_uuid, new_uuid = %new_uuid, name = %trimmed, "User profile duplicated");
    Ok(new_uuid)
}

/// Delete an existing user profile.
///
/// Rejects:
/// - [`DEFAULT_USER_PROFILE_UUID`] ([`UserProfileMutationError::Reserved`]).
/// - Any UUID currently referenced by a TV's `profile.user`
///   ([`UserProfileMutationError::InUse`]); the error message names
///   the offending TV UUIDs.
/// - Unknown UUIDs ([`UserProfileMutationError::Failed`]).
pub async fn delete_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
) -> Result<(), UserProfileMutationError> {
    if uuid == DEFAULT_USER_PROFILE_UUID {
        return Err(UserProfileMutationError::Reserved(
            "the default user profile cannot be deleted".into(),
        ));
    }

    let (name, tvs) = {
        let s = state.read().await;
        let Some(body) = s.user_config.user.profiles.get(&uuid) else {
            return Err(UserProfileMutationError::Failed(format!(
                "Unknown user profile uuid: {}",
                uuid
            )));
        };
        (body.name.clone(), user_profile_referenced_by(&s, uuid))
    };

    if !tvs.is_empty() {
        return Err(UserProfileMutationError::InUse { name, tvs });
    }

    {
        let mut s = state.write().await;
        s.user_config.user.profiles.remove(&uuid);
    }

    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;
    info!(uuid = %uuid, "User profile deleted");
    Ok(())
}

/// Clear every field on a user profile back to `None`. `name` is
/// preserved; the default-UUID profile's name is locked to
/// [`DEFAULT_PROFILE_NAME`] by [`tisvlay_core::UserSection::ensure_default`].
pub async fn reset_user_profile(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
    uuid: Uuid,
) -> Result<(), UserProfileMutationError> {
    {
        let mut s = state.write().await;
        let body = s.user_config.user.profiles.get_mut(&uuid).ok_or_else(|| {
            UserProfileMutationError::Failed(format!("Unknown user profile uuid: {}", uuid))
        })?;
        body.on_session_lock = None;
        body.on_session_unlock = None;
        body.on_greeter_wake = None;
        body.on_idle_timeout = None;
        body.on_idle_resume = None;
        body.idle_timeout_seconds = None;
    }
    refresh_all_resolved_user_profiles(state).await;
    persist_user_config(state, config_path).await?;
    debug!(uuid = %uuid, "User profile reset");
    Ok(())
}

/// Names of the fields this profile explicitly sets (each
/// `Option<_>` field that is `Some`), including `idle_timeout_seconds`.
pub fn user_profile_locally_set_fields(body: &UserProfileConfig) -> Vec<String> {
    let mut fields = Vec::new();
    if body.on_session_lock.is_some() {
        fields.push("on_session_lock".to_string());
    }
    if body.on_session_unlock.is_some() {
        fields.push("on_session_unlock".to_string());
    }
    if body.on_greeter_wake.is_some() {
        fields.push("on_greeter_wake".to_string());
    }
    if body.on_idle_timeout.is_some() {
        fields.push("on_idle_timeout".to_string());
    }
    if body.on_idle_resume.is_some() {
        fields.push("on_idle_resume".to_string());
    }
    if body.idle_timeout_seconds.is_some() {
        fields.push("idle_timeout_seconds".to_string());
    }
    fields
}

/// UUIDs of TVs whose `profile.user` references this profile. Direct
/// UUID equality check — references are UUID-keyed end-to-end.
pub fn user_profile_referenced_by(state: &SessionState, uuid: Uuid) -> Vec<String> {
    if !state.user_config.user.profiles.contains_key(&uuid) {
        return Vec::new();
    }
    state
        .user_config
        .tv
        .iter()
        .filter(|t| t.profile.user == uuid)
        .map(|t| t.uuid.clone())
        .collect()
}

/// Resolve a user profile by UUID, walking the inheritance chain
/// against the cached admin baseline.
pub fn resolve_user_profile_by_uuid(
    state: &SessionState,
    uuid: Uuid,
) -> tisvlay_core::ResolvedUserProfile {
    state.user_config.user.resolve_profile_by_uuid(
        uuid,
        &state.admin_user_default,
        &state.admin_user_profiles,
    )
}

/// Re-resolve every TV's user profile against the current user config
/// plus admin baseline. Called after any in-memory profile mutation so
/// downstream consumers (event handler, idle monitor) observe the new
/// resolved values without waiting for a `ReloadConfig` round-trip.
async fn refresh_all_resolved_user_profiles(state: &Arc<RwLock<SessionState>>) {
    let mut s = state.write().await;
    let tv_ids: Vec<String> = s.tv_ids.clone();
    let admin_default = s.admin_user_default.clone();
    for tv_id in tv_ids {
        let resolved = s.user_config.resolve_user_profile(&tv_id, &admin_default);
        s.profiles.insert(tv_id, resolved);
    }
}

/// Persist the user config to disk (the path the SessionService was
/// constructed with, or the XDG default).
async fn persist_user_config(
    state: &Arc<RwLock<SessionState>>,
    config_path: Option<&PathBuf>,
) -> Result<(), UserProfileMutationError> {
    let s = state.read().await;
    let result = match config_path {
        Some(path) => s.user_config.save_to(path),
        None => s.user_config.save(),
    };
    result
        .map_err(|e| UserProfileMutationError::Failed(format!("Failed to save user config: {}", e)))
}

/// Refresh cached typed fields on one `UserProfile1` interface.
///
/// Re-resolves the profile against the current inheritance chain,
/// re-reads `LocallySetFields` + `ReferencedBy`, and writes any
/// changed values back via `iface_ref.get_mut()`. zbus's macro emits
/// `PropertiesChanged` for each `<field>_changed()` call we make.
///
/// The "did the value actually change?" guards keep the signal count
/// down so clients don't see spurious refreshes for properties whose
/// resolved value didn't shift.
pub async fn refresh_user_profile_cached_fields(
    object_server: &zbus::ObjectServer,
    state: &Arc<RwLock<SessionState>>,
    uuid: Uuid,
) {
    let path = user_profile_path(&uuid);
    let Ok(iface_ref) = object_server
        .interface::<_, UserProfileService>(&*path)
        .await
    else {
        return;
    };

    let (resolved, new_name, new_locally_set, new_referenced_by) = {
        let s = state.read().await;
        let resolved = resolve_user_profile_by_uuid(&s, uuid);
        let new_name = s
            .user_config
            .user
            .profiles
            .get(&uuid)
            .map(|b| b.name.clone());
        let new_locally_set = s
            .user_config
            .user
            .profiles
            .get(&uuid)
            .map(user_profile_locally_set_fields)
            .unwrap_or_default();
        let new_referenced_by = user_profile_referenced_by(&s, uuid);
        (resolved, new_name, new_locally_set, new_referenced_by)
    };

    let mut iface = iface_ref.get_mut().await;
    let emitter = iface_ref.signal_emitter();

    if let Some(name) = new_name
        && iface.name != name
    {
        iface.name = name;
        iface.name_changed(emitter).await.ok();
    }

    let new_on_session_lock = resolved.on_session_lock.to_string();
    if iface.on_session_lock != new_on_session_lock {
        iface.on_session_lock = new_on_session_lock;
        iface.on_session_lock_changed(emitter).await.ok();
    }
    let new_on_session_unlock = resolved.on_session_unlock.to_string();
    if iface.on_session_unlock != new_on_session_unlock {
        iface.on_session_unlock = new_on_session_unlock;
        iface.on_session_unlock_changed(emitter).await.ok();
    }
    let new_on_greeter_wake = resolved.on_greeter_wake.to_string();
    if iface.on_greeter_wake != new_on_greeter_wake {
        iface.on_greeter_wake = new_on_greeter_wake;
        iface.on_greeter_wake_changed(emitter).await.ok();
    }
    let new_on_idle_timeout = resolved.on_idle_timeout.to_string();
    if iface.on_idle_timeout != new_on_idle_timeout {
        iface.on_idle_timeout = new_on_idle_timeout;
        iface.on_idle_timeout_changed(emitter).await.ok();
    }
    let new_on_idle_resume = resolved.on_idle_resume.to_string();
    if iface.on_idle_resume != new_on_idle_resume {
        iface.on_idle_resume = new_on_idle_resume;
        iface.on_idle_resume_changed(emitter).await.ok();
    }
    // u64 → u32 cast: idle timeouts realistically fit in u32 (max
    // ~136 years). Cap at u32::MAX defensively rather than wrap.
    let new_idle_timeout: u32 = resolved.idle_timeout_seconds.min(u32::MAX as u64) as u32;
    if iface.idle_timeout_seconds != new_idle_timeout {
        iface.idle_timeout_seconds = new_idle_timeout;
        iface.idle_timeout_seconds_changed(emitter).await.ok();
    }
    if iface.locally_set_fields != new_locally_set {
        iface.locally_set_fields = new_locally_set;
        iface.locally_set_fields_changed(emitter).await.ok();
    }
    if iface.referenced_by != new_referenced_by {
        iface.referenced_by = new_referenced_by;
        iface.referenced_by_changed(emitter).await.ok();
    }
}

/// Forward user-profile body-change events to cached D-Bus properties.
///
/// Mirrors the daemon's `state_change_forwarder` arm for
/// `SystemProfileBodyChanged`: when the default UUID is the target,
/// every other profile's resolved chain shifted, so refresh the whole
/// catalog. Otherwise refresh just the affected UUID. The
/// `TvReassigned` arm refreshes `ReferencedBy` on every UUID in the
/// affected set.
pub async fn user_profile_state_forwarder(
    connection: zbus::Connection,
    state: Arc<RwLock<SessionState>>,
    mut rx: mpsc::UnboundedReceiver<UserProfileEvent>,
) {
    debug!("User profile state forwarder started");
    let object_server = connection.object_server();

    while let Some(event) = rx.recv().await {
        match event {
            UserProfileEvent::BodyChanged { uuid } => {
                debug!(profile = %uuid, "Forwarding user profile body change to D-Bus");
                if uuid == DEFAULT_USER_PROFILE_UUID {
                    let all: Vec<Uuid> = {
                        let s = state.read().await;
                        s.user_config.user.profiles.keys().copied().collect()
                    };
                    for affected in all {
                        refresh_user_profile_cached_fields(object_server, &state, affected).await;
                    }
                } else {
                    refresh_user_profile_cached_fields(object_server, &state, uuid).await;
                }
            }
            UserProfileEvent::TvReassigned { affected_uuids } => {
                debug!(
                    count = affected_uuids.len(),
                    "Forwarding TV user-profile reassignment to D-Bus"
                );
                for uuid in affected_uuids {
                    refresh_user_profile_cached_fields(object_server, &state, uuid).await;
                }
            }
        }
    }

    debug!("User profile state forwarder stopped");
}

/// Register the `UserProfile1` interface for one profile at its
/// UUID-derived object path. Defensive `remove` first — same rationale
/// as the per-TV path (zbus 5.x `at()` returns `Ok(false)` instead of
/// erroring on a registered path, so a stale interface would silently
/// keep serving without this flush).
///
/// Cached fields are populated from the current resolved state.
pub async fn register_user_profile(
    connection: &zbus::Connection,
    state: Arc<RwLock<SessionState>>,
    config_path: Option<PathBuf>,
    events: UserProfileEventTx,
    uuid: Uuid,
) -> zbus::Result<()> {
    let path = user_profile_path(&uuid);
    let object_server = connection.object_server();
    let _ = object_server
        .remove::<UserProfileService, _>(path.as_str())
        .await;

    let (resolved, name, locally_set_fields, referenced_by) = {
        let s = state.read().await;
        let resolved = resolve_user_profile_by_uuid(&s, uuid);
        let name = s
            .user_config
            .user
            .profiles
            .get(&uuid)
            .map(|b| b.name.clone())
            .unwrap_or_else(|| {
                // Default UUID always present (ensure_default); fall back
                // to the reserved name to keep the wire surface non-empty
                // if a race ever lands here. Non-default UUIDs without a
                // body shouldn't reach this path (registration is gated
                // on the catalog).
                if uuid == DEFAULT_USER_PROFILE_UUID {
                    DEFAULT_PROFILE_NAME.to_string()
                } else {
                    String::new()
                }
            });
        let locally_set_fields = s
            .user_config
            .user
            .profiles
            .get(&uuid)
            .map(user_profile_locally_set_fields)
            .unwrap_or_default();
        let referenced_by = user_profile_referenced_by(&s, uuid);
        (resolved, name, locally_set_fields, referenced_by)
    };

    let idle_timeout_seconds: u32 = resolved.idle_timeout_seconds.min(u32::MAX as u64) as u32;

    object_server
        .at(
            path.as_str(),
            UserProfileService {
                uuid,
                state,
                config_path,
                events,
                name,
                on_session_lock: resolved.on_session_lock.to_string(),
                on_session_unlock: resolved.on_session_unlock.to_string(),
                on_greeter_wake: resolved.on_greeter_wake.to_string(),
                on_idle_timeout: resolved.on_idle_timeout.to_string(),
                on_idle_resume: resolved.on_idle_resume.to_string(),
                idle_timeout_seconds,
                locally_set_fields,
                referenced_by,
            },
        )
        .await?;
    Ok(())
}

/// Unregister the `UserProfile1` interface for one profile UUID.
pub async fn unregister_user_profile(connection: &zbus::Connection, uuid: Uuid) {
    let path = user_profile_path(&uuid);
    let _ = connection
        .object_server()
        .remove::<UserProfileService, _>(path.as_str())
        .await;
}
