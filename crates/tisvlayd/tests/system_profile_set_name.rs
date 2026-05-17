//! `Properties.Set` on `SystemProfile1.Name` renames the profile in
//! place. The UUID-keyed object path stays the same, so
//! `GetManagedObjects` still finds the profile. Reserved-name + cross
//! profile uniqueness rules are enforced server-side as typed errors.

#![cfg(feature = "mock")]

use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SystemConfig,
    SystemProfileConfig, system_profile_path,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

fn config_with_default_and_guest(guest_uuid: Uuid) -> SystemConfig {
    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..Default::default()
        },
    );
    config.system.profiles.insert(
        guest_uuid,
        SystemProfileConfig {
            name: "guest".to_string(),
            ..Default::default()
        },
    );
    config
}

async fn props_proxy<'a>(daemon: &'a TestDaemon, uuid: Uuid) -> zbus::fdo::PropertiesProxy<'a> {
    zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy")
}

#[tokio::test]
async fn rename_updates_name_and_keeps_object_path() {
    let guest_uuid = Uuid::new_v4();
    let daemon = TestDaemon::with_config(config_with_default_and_guest(guest_uuid))
        .await
        .expect("daemon");

    let proxy = props_proxy(&daemon, guest_uuid).await;
    proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "Name",
            zbus::zvariant::Value::from("renamed_guest"),
        )
        .await
        .expect("Set Name");

    // Cached Name reflects the rename.
    let new_name: String = proxy
        .get("org.tisvlay.SystemProfile1".try_into().unwrap(), "Name")
        .await
        .expect("Get Name")
        .try_into()
        .expect("string");
    assert_eq!(new_name, "renamed_guest");

    // GetManagedObjects still finds the profile at the same path.
    let object_manager = zbus::fdo::ObjectManagerProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
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
    let expected = system_profile_path(&guest_uuid);
    let key = managed.keys().find(|p| p.as_str() == expected);
    assert!(
        key.is_some(),
        "renamed profile must still be visible at {} via GetManagedObjects",
        expected,
    );
}

#[tokio::test]
async fn rename_to_default_on_non_default_profile_is_rejected() {
    let guest_uuid = Uuid::new_v4();
    let daemon = TestDaemon::with_config(config_with_default_and_guest(guest_uuid))
        .await
        .expect("daemon");

    let proxy = props_proxy(&daemon, guest_uuid).await;
    let err = proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
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
}

#[tokio::test]
async fn rename_default_profile_is_rejected() {
    let guest_uuid = Uuid::new_v4();
    let daemon = TestDaemon::with_config(config_with_default_and_guest(guest_uuid))
        .await
        .expect("daemon");

    let proxy = props_proxy(&daemon, DEFAULT_SYSTEM_PROFILE_UUID).await;
    let err = proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
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
}
