//! Per-profile D-Bus interface stub for system profiles.
//!
//! Only the `Id` const property + the object-path registration land in
//! this commit. Typed field properties (`Name`, `OnStartup`, …) follow
//! in subsequent commits per the profile-CRUD-v2 plan. Lifecycle rides
//! on the existing `/org/tisvlay` ObjectManager — `InterfacesAdded` /
//! `InterfacesRemoved` are emitted as a side effect of the
//! `serve_at` / `remove` calls below, matching the per-TV pattern.
//!
//! `register_system_profile` / `unregister_system_profile` are the
//! single chokepoints; the daemon's startup wiring iterates over every
//! profile in the config, and `apply_reload_diff` mirrors the TV-side
//! diff logic when `Manager1.ReloadConfig` rebuilds the catalog.

use tisvlay_core::dbus_paths::system_profile_path;
use uuid::Uuid;
use zbus::interface;

#[derive(Debug)]
pub struct SystemProfileService {
    pub uuid: Uuid,
}

#[interface(name = "org.tisvlay.SystemProfile1")]
impl SystemProfileService {
    /// Profile UUID. Const for the lifetime of the object — the daemon
    /// re-registers the object at the same path on `ReloadConfig`, so
    /// `Id` is effectively immutable from the client's perspective.
    #[zbus(property)]
    fn id(&self) -> String {
        self.uuid.to_string()
    }
}

/// Register the `SystemProfile1` interface for one profile at its
/// UUID-derived object path. Defensive `remove` first — same rationale
/// as the per-TV path (zbus 5.x `at()` returns `Ok(false)` instead of
/// erroring on a registered path, so a stale interface would silently
/// keep serving without this flush).
pub async fn register_system_profile(
    connection: &zbus::Connection,
    uuid: Uuid,
) -> zbus::Result<()> {
    let path = system_profile_path(&uuid);
    let object_server = connection.object_server();
    let _ = object_server
        .remove::<SystemProfileService, _>(path.as_str())
        .await;
    object_server
        .at(path.as_str(), SystemProfileService { uuid })
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
