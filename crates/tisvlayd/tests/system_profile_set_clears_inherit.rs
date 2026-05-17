//! `Properties.Set` on a typed `SystemProfile1` field promotes the
//! field to locally-set even when the value matches the inherited
//! fallthrough. "I explicitly picked this" semantics — `ResetField`
//! is the way to go back to inheriting.

#![cfg(feature = "mock")]

use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SessionAction, SystemConfig,
    SystemProfileConfig, system_profile_path,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

#[tokio::test]
async fn set_with_inherited_value_promotes_to_locally_set() {
    let guest_uuid = Uuid::new_v4();
    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_startup: Some(SessionAction::PowerOn),
            ..Default::default()
        },
    );
    config.system.profiles.insert(
        guest_uuid,
        SystemProfileConfig {
            name: "guest".to_string(),
            // Guest inherits on_startup → resolved value is power_on.
            ..Default::default()
        },
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    let proxy = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&guest_uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    // Sanity: guest inherits on_startup from default ("power_on").
    let resolved: zbus::zvariant::OwnedValue = proxy
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
        )
        .await
        .expect("Get OnStartup")
        .try_to_owned()
        .expect("OwnedValue");
    let resolved_s: String = resolved.try_into().expect("string");
    assert_eq!(resolved_s, "power_on");

    // Pre-condition: guest doesn't list on_startup as locally-set.
    let pre: zbus::zvariant::OwnedValue = proxy
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get LocallySetFields")
        .try_to_owned()
        .expect("OwnedValue");
    let pre_list: Vec<String> = pre.try_into().expect("string array");
    assert!(
        !pre_list.contains(&"on_startup".to_string()),
        "pre-condition: on_startup must not be locally-set before the Set; got {:?}",
        pre_list,
    );

    // Set OnStartup to the *same* value it currently resolves to.
    // The act of setting should still promote it to locally-set —
    // the daemon stores it as Some(_) regardless of equality with
    // the inherited fallthrough.
    proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
            zbus::zvariant::Value::from("power_on"),
        )
        .await
        .expect("Properties.Set with inherited value");

    let post: zbus::zvariant::OwnedValue = proxy
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get LocallySetFields")
        .try_to_owned()
        .expect("OwnedValue");
    let post_list: Vec<String> = post.try_into().expect("string array");
    assert!(
        post_list.contains(&"on_startup".to_string()),
        "after Set, on_startup must be locally-set even though the value matched the inherit fallthrough; got {:?}",
        post_list,
    );
}
