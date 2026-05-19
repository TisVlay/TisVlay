//! Per-profile D-Bus interface for system profiles.
//!
//! One `SystemProfile1` object lives at
//! `/org/tisvlay/SystemProfile/<UUID_>` per system profile in the
//! catalog. Each typed field property reports the **resolved** value
//! after walking the inheritance chain (this profile →
//! [`DEFAULT_SYSTEM_PROFILE_UUID`] → hardcoded baseline);
//! `LocallySetFields` names the fields the user has explicitly set
//! versus inheriting. Editing rides on standard `Properties.Set` +
//! `Properties.PropertiesChanged` rather than custom JSON RPCs —
//! the same pattern the per-TV interfaces use.
//!
//! Cached fields are populated at registration and kept fresh by the
//! `state_forwarder`'s `SystemProfileBodyChanged` handler — same
//! mechanic the per-TV `OptionChanged` path uses.
//!
//! Mutation routes through [`ProfileManager`] (implemented for
//! [`TvManager`](crate::tv_manager::TvManager)) so the per-property
//! setters' bodies have the same shape as the session-side
//! `UserProfile1` setters — both call `manager.set_string_field`.

use crate::tv_manager::TvManager;
use std::sync::Arc;
use tisvlay_core::dbus_paths::system_profile_path;
use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, ProfileError, ProfileManager,
};
use tracing::info;
use uuid::Uuid;
use zbus::interface;

/// Cached state for one `SystemProfile1` D-Bus object.
///
/// Mirrors the per-TV `TvService` shape: typed string fields back the
/// `#[zbus(property)]` getters, `&mut self` setters mutate state via
/// the manager and update the cache so zbus's macro emits
/// `PropertiesChanged` automatically.
#[derive(Debug)]
pub struct SystemProfileService {
    pub uuid: Uuid,
    pub manager: Arc<TvManager>,
    pub name: String,
    pub on_startup: String,
    pub on_shutdown: String,
    pub on_reboot: String,
    pub on_no_session: String,
    pub on_sleep: String,
    pub on_wake: String,
    pub automate_when: String,
    pub locally_set_fields: Vec<String>,
    pub referenced_by: Vec<String>,
}

#[interface(name = "org.tisvlay.SystemProfile1")]
impl SystemProfileService {
    /// Profile UUID. Const for the lifetime of the object — the daemon
    /// re-registers the object at the same path on `ReloadConfig`, so
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

    /// Set the friendly profile name. Validates non-empty +
    /// uniqueness + reserved-name rules on the manager side. The
    /// default-UUID profile is locked to `"default"`; non-default
    /// profiles cannot be renamed to `"default"`.
    #[zbus(property)]
    async fn set_name(&mut self, name: String) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, name = %name, "D-Bus: SystemProfile1.set Name");
        self.manager
            .set_string_field(self.uuid, "name", &name)
            .await
            .map_err(profile_err)?;
        self.name = name;
        Ok(())
    }

    /// Resolved `on_startup` action (post inheritance walk).
    #[zbus(property)]
    fn on_startup(&self) -> &str {
        &self.on_startup
    }

    /// Set `on_startup`. Promotes the field to locally-set even if
    /// the value matches the inherited fallthrough — explicit user
    /// intent ("I picked this") is preserved across edits. Use
    /// `ResetField` to go back to inheriting.
    #[zbus(property)]
    async fn set_on_startup(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_startup", value).await
    }

    /// Resolved `on_shutdown` action.
    #[zbus(property)]
    fn on_shutdown(&self) -> &str {
        &self.on_shutdown
    }

    #[zbus(property)]
    async fn set_on_shutdown(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_shutdown", value).await
    }

    /// Resolved `on_reboot` action.
    #[zbus(property)]
    fn on_reboot(&self) -> &str {
        &self.on_reboot
    }

    #[zbus(property)]
    async fn set_on_reboot(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_reboot", value).await
    }

    /// Resolved `on_no_session` action.
    #[zbus(property)]
    fn on_no_session(&self) -> &str {
        &self.on_no_session
    }

    #[zbus(property)]
    async fn set_on_no_session(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_no_session", value).await
    }

    /// Resolved `on_sleep` action.
    #[zbus(property)]
    fn on_sleep(&self) -> &str {
        &self.on_sleep
    }

    #[zbus(property)]
    async fn set_on_sleep(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_sleep", value).await
    }

    /// Resolved `on_wake` action.
    #[zbus(property)]
    fn on_wake(&self) -> &str {
        &self.on_wake
    }

    #[zbus(property)]
    async fn set_on_wake(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("on_wake", value).await
    }

    /// Resolved `automate_when` policy.
    #[zbus(property)]
    fn automate_when(&self) -> &str {
        &self.automate_when
    }

    #[zbus(property)]
    async fn set_automate_when(&mut self, value: String) -> zbus::fdo::Result<()> {
        self.set_field("automate_when", value).await
    }

    /// Names of the fields this profile explicitly sets. Drives the
    /// GUI's gutter accent + reset-glyph visibility. Refresh fans out
    /// automatically when any setter completes — see
    /// [`crate::tv_manager::StateChangeEvent::SystemProfileBodyChanged`].
    #[zbus(property)]
    fn locally_set_fields(&self) -> Vec<String> {
        self.locally_set_fields.clone()
    }

    /// UUIDs of TVs that reference this profile via
    /// `Tv1.SystemProfile`.
    #[zbus(property)]
    fn referenced_by(&self) -> Vec<String> {
        self.referenced_by.clone()
    }

    /// Clear one field back to inheriting. Triggers
    /// `PropertiesChanged` on both the field's property (resolved
    /// value may shift) and `LocallySetFields`. The cache refresh
    /// rides on the `SystemProfileBodyChanged` fan-out from the
    /// manager, not on this setter directly.
    async fn reset_field(&self, field: &str) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, field = %field, "D-Bus: SystemProfile1.ResetField");
        ProfileManager::reset_field(&*self.manager, self.uuid, field)
            .await
            .map_err(profile_err)
    }

    /// Clear every field back to inheriting. One round-trip; the
    /// fan-out fires `PropertiesChanged` for every property whose
    /// cached value shifts.
    async fn reset_all(&self) -> zbus::fdo::Result<()> {
        info!(uuid = %self.uuid, "D-Bus: SystemProfile1.ResetAll");
        self.manager.reset_all(self.uuid).await.map_err(profile_err)
    }
}

impl SystemProfileService {
    /// Setter helper for the seven typed-field setters. Routes
    /// through [`ProfileManager::set_string_field`] and updates
    /// `self.<field>` on success so zbus's `#[interface]` macro
    /// emits `<field>_changed()` automatically. The cross-profile
    /// fan-out (default-edit shifting every other profile's
    /// resolved value) rides through the state-forwarder loop.
    async fn set_field(&mut self, field: &str, value: String) -> zbus::fdo::Result<()> {
        info!(
            uuid = %self.uuid,
            field = %field,
            value = %value,
            "D-Bus: SystemProfile1.set field"
        );
        self.manager
            .set_string_field(self.uuid, field, &value)
            .await
            .map_err(profile_err)?;
        match field {
            "on_startup" => self.on_startup = value,
            "on_shutdown" => self.on_shutdown = value,
            "on_reboot" => self.on_reboot = value,
            "on_no_session" => self.on_no_session = value,
            "on_sleep" => self.on_sleep = value,
            "on_wake" => self.on_wake = value,
            "automate_when" => self.automate_when = value,
            _ => {}
        }
        Ok(())
    }
}

/// Map a [`ProfileError`] to a `zbus::fdo` error name. `Invalid` →
/// `InvalidArgs` so clients can route on the standard error name
/// without parsing the body string.
fn profile_err(e: ProfileError) -> zbus::fdo::Error {
    match e {
        ProfileError::Invalid(msg) => zbus::fdo::Error::InvalidArgs(msg),
        ProfileError::Failed(msg) => zbus::fdo::Error::Failed(msg),
        ProfileError::AccessDenied(msg) => zbus::fdo::Error::AccessDenied(msg),
    }
}

/// Register the `SystemProfile1` interface for one profile at its
/// UUID-derived object path. Defensive `remove` first — same rationale
/// as the per-TV path (zbus 5.x `at()` returns `Ok(false)` instead of
/// erroring on a registered path, so a stale interface would silently
/// keep serving without this flush).
///
/// Cached fields are populated from the manager's current state. The
/// state-forwarder is responsible for keeping them fresh going
/// forward via `SystemProfileBodyChanged` events.
pub async fn register_system_profile(
    connection: &zbus::Connection,
    uuid: Uuid,
    manager: Arc<TvManager>,
) -> zbus::Result<()> {
    let path = system_profile_path(&uuid);
    let object_server = connection.object_server();
    let _ = object_server
        .remove::<SystemProfileService, _>(path.as_str())
        .await;

    let resolved = manager.resolve_system_profile_by_uuid(uuid).await;
    let body = manager.get_system_profile_body(uuid).await;
    let name = body.map(|b| b.name).unwrap_or_else(|| {
        // The default UUID is always present (ensure_default) and its
        // name is locked to DEFAULT_PROFILE_NAME; falling back to that
        // string for the default avoids a "" name on the wire if the
        // body lookup ever races. Other UUIDs without a body should
        // never reach this path (the registration is gated on the
        // catalog), but the fallback keeps the surface non-empty.
        if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
            DEFAULT_PROFILE_NAME.to_string()
        } else {
            String::new()
        }
    });
    let locally_set_fields = manager.system_profile_locally_set_fields(uuid).await;
    let referenced_by = manager.system_profile_referenced_by(uuid).await;

    object_server
        .at(
            path.as_str(),
            SystemProfileService {
                uuid,
                manager,
                name,
                on_startup: resolved.on_startup.to_string(),
                on_shutdown: resolved.on_shutdown.to_string(),
                on_reboot: resolved.on_reboot.to_string(),
                on_no_session: resolved.on_no_session.to_string(),
                on_sleep: resolved.on_sleep.to_string(),
                on_wake: resolved.on_wake.to_string(),
                automate_when: resolved.automate_when.to_string(),
                locally_set_fields,
                referenced_by,
            },
        )
        .await?;
    Ok(())
}

/// Unregister the `SystemProfile1` interface for one profile.
/// Infallible by construction (errors are swallowed); matches
/// `unregister_tv_interfaces`.
pub async fn unregister_system_profile(connection: &zbus::Connection, uuid: Uuid) {
    let path = system_profile_path(&uuid);
    let _ = connection
        .object_server()
        .remove::<SystemProfileService, _>(path.as_str())
        .await;
}
