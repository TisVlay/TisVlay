//! The daemon must register a `SystemProfile1` object at
//! `/org/tisvlay/SystemProfile/<UUID_>` for every system profile in
//! the loaded config. `ObjectManager.GetManagedObjects` is the
//! discovery surface the GUI's Profiles pane will use to enumerate
//! profiles, so this verifies the foundation.

#![cfg(feature = "mock")]

use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SystemProfileConfig,
    system_profile_path,
};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config};

#[tokio::test]
async fn default_system_profile_is_visible_via_object_manager() {
    let mut config = mock_tv_config("foo", true, None);
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..SystemProfileConfig::full_default()
        },
    );

    let daemon = TestDaemon::with_config(config).await.expect("test daemon");

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

    let expected_path = system_profile_path(&DEFAULT_SYSTEM_PROFILE_UUID);
    let key = managed
        .keys()
        .find(|p| p.as_str() == expected_path)
        .unwrap_or_else(|| {
            panic!(
                "expected {} in managed objects; got {:?}",
                expected_path,
                managed.keys().map(|k| k.to_string()).collect::<Vec<_>>(),
            )
        });

    let interfaces = managed.get(key).expect("entry for system profile path");
    let id_props = interfaces
        .get("org.tisvlay.SystemProfile1")
        .expect("SystemProfile1 interface should be present at the profile path");

    let id_value = id_props
        .get("Id")
        .expect("Id property should be present on SystemProfile1");
    let id_str: String = id_value
        .try_to_owned()
        .expect("Id is owned-convertible")
        .try_into()
        .expect("Id is a string");
    assert_eq!(
        id_str,
        DEFAULT_SYSTEM_PROFILE_UUID.to_string(),
        "Id property should be the profile's hyphenated UUID",
    );
}
