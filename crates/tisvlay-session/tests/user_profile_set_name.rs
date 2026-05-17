//! `Properties.Set` on `UserProfile1.Name` renames the profile in
//! place. The UUID-keyed object path stays the same, so
//! `GetManagedObjects` still finds the profile. Reserved-name +
//! cross-profile uniqueness rules are enforced server-side as typed
//! errors.

use std::path::PathBuf;

use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, UserConfig,
    UserProfileConfig, user_profile_path,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

fn config_with_default_and_gaming(gaming_uuid: Uuid) -> UserConfig {
    let mut config = UserConfig::default();
    config.user.profiles.insert(
        DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..Default::default()
        },
    );
    config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            ..Default::default()
        },
    );
    config
}

async fn props_proxy<'a>(conn: &'a zbus::Connection, uuid: Uuid) -> zbus::fdo::PropertiesProxy<'a> {
    zbus::fdo::PropertiesProxy::builder(conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path(user_profile_path(&uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy")
}

#[tokio::test]
async fn rename_updates_name_and_keeps_object_path() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    config_with_default_and_gaming(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("rename_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = props_proxy(&conn, gaming_uuid).await;

    proxy
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "Name",
            zbus::zvariant::Value::from("retro_gaming"),
        )
        .await
        .expect("Set Name");

    // Cached Name reflects the rename.
    let new_name: String = proxy
        .get("org.tisvlay.UserProfile1".try_into().unwrap(), "Name")
        .await
        .expect("Get Name")
        .try_into()
        .expect("string");
    assert_eq!(new_name, "retro_gaming");

    // GetManagedObjects still finds the profile at the same path.
    let object_manager = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path("/org/tisvlay")
        .expect("path")
        .build()
        .await
        .expect("ObjectManager proxy");
    let managed = object_manager
        .get_managed_objects()
        .await
        .expect("GetManagedObjects");
    let expected = user_profile_path(&gaming_uuid);
    let key = managed.keys().find(|p| p.as_str() == expected);
    assert!(
        key.is_some(),
        "renamed profile must still be visible at {} via GetManagedObjects",
        expected,
    );

    let _ = session;
}

#[tokio::test]
async fn rename_to_default_on_non_default_profile_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    config_with_default_and_gaming(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("rename_dflt_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = props_proxy(&conn, gaming_uuid).await;

    let err = proxy
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "Name",
            zbus::zvariant::Value::from(DEFAULT_PROFILE_NAME),
        )
        .await
        .expect_err("Set Name to reserved 'default' should fail");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("default") || msg.contains("reserved"),
        "error message should mention the reserved name; got: {err:?}",
    );

    let _ = session;
}

#[tokio::test]
async fn rename_default_profile_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    let gaming_uuid = Uuid::new_v4();
    config_with_default_and_gaming(gaming_uuid)
        .save_to(&config_path)
        .unwrap();

    let session = TestSession::with_user_config_path("rename_def2_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = props_proxy(&conn, DEFAULT_USER_PROFILE_UUID).await;

    let err = proxy
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "Name",
            zbus::zvariant::Value::from("not_default"),
        )
        .await
        .expect_err("renaming the default-UUID profile should fail");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("default") || msg.contains("rename"),
        "error message should mention the default invariant; got: {err:?}",
    );

    let _ = session;
}
