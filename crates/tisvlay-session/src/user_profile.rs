//! Per-profile D-Bus interface stub for user profiles.
//!
//! Mirrors the daemon-side `SystemProfile1` registration (only the
//! `Id` const property + object-path lifecycle in this commit; typed
//! field properties land in a later commit per the profile-CRUD-v2
//! plan).

use tisvlay_core::dbus_paths::user_profile_path;
use uuid::Uuid;
use zbus::interface;

#[derive(Debug)]
pub struct UserProfileService {
    pub uuid: Uuid,
}

#[interface(name = "org.tisvlay.UserProfile1")]
impl UserProfileService {
    /// Profile UUID. Const for the lifetime of the object.
    #[zbus(property)]
    fn id(&self) -> String {
        self.uuid.to_string()
    }
}

/// Register the `UserProfile1` interface for one profile UUID.
pub async fn register_user_profile(connection: &zbus::Connection, uuid: Uuid) -> zbus::Result<()> {
    let path = user_profile_path(&uuid);
    let object_server = connection.object_server();
    let _ = object_server
        .remove::<UserProfileService, _>(path.as_str())
        .await;
    object_server
        .at(path.as_str(), UserProfileService { uuid })
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
