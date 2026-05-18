//! `Manager1.DuplicateSystemProfile(source_uuid, new_name)` clones a
//! profile's locally-set fields under a fresh UUID + new name.
//! Inheriting fields stay inheriting; `ReferencedBy` starts empty.

#![cfg(feature = "mock")]

use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SessionAction, SystemConfig,
    SystemProfileConfig,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

async fn manager_proxy<'a>(daemon: &'a TestDaemon) -> zbus::Proxy<'a> {
    zbus::proxy::Builder::new(daemon.connection())
        .destination(DAEMON_SERVICE)
        .unwrap()
        .path("/org/tisvlay/Manager")
        .unwrap()
        .interface("org.tisvlay.Manager1")
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn config_with_guest(guest_uuid: Uuid) -> SystemConfig {
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
            // Locally set; the copy must preserve this.
            on_startup: Some(SessionAction::PowerOff),
            // Inherits on_shutdown — copy must keep it inheriting.
            ..Default::default()
        },
    );
    config
}

#[tokio::test]
async fn duplicate_preserves_locally_set_fields_and_inheritance() {
    let guest_uuid = Uuid::new_v4();
    let daemon = TestDaemon::with_config(config_with_guest(guest_uuid))
        .await
        .expect("test daemon");
    let manager = manager_proxy(&daemon).await;

    let new_path: zbus::zvariant::OwnedObjectPath = manager
        .call(
            "DuplicateSystemProfile",
            &(guest_uuid.to_string(), "guest-copy"),
        )
        .await
        .expect("DuplicateSystemProfile");

    assert!(
        new_path.as_str().starts_with("/org/tisvlay/SystemProfile/"),
        "expected SystemProfile path, got {}",
        new_path.as_str()
    );

    let object_manager = zbus::fdo::ObjectManagerProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .unwrap()
        .path("/org/tisvlay")
        .unwrap()
        .build()
        .await
        .unwrap();
    let managed = object_manager.get_managed_objects().await.unwrap();
    let entry = managed
        .iter()
        .find(|(p, _)| p.as_str() == new_path.as_str())
        .expect("new profile present in GetManagedObjects");
    let props = entry
        .1
        .get("org.tisvlay.SystemProfile1")
        .expect("SystemProfile1 interface present");

    let name: String = props
        .get("Name")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(name, "guest-copy");

    let new_uuid: String = props
        .get("Id")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_ne!(
        new_uuid,
        guest_uuid.to_string(),
        "duplicate must mint a fresh UUID"
    );

    let locally_set: Vec<String> = props
        .get("LocallySetFields")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        locally_set,
        vec!["on_startup".to_string()],
        "copy must carry the same locally-set fields as the source"
    );

    let on_startup: String = props
        .get("OnStartup")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(on_startup, "power_off", "locally-set value preserved");

    let on_shutdown: String = props
        .get("OnShutdown")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        on_shutdown, "power_off",
        "inheriting field still resolves through the chain to its hardcoded default"
    );

    let referenced_by: Vec<String> = props
        .get("ReferencedBy")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert!(
        referenced_by.is_empty(),
        "fresh duplicate has no TV references; got {referenced_by:?}",
    );
}

#[tokio::test]
async fn duplicate_rejects_collision_with_existing_name() {
    let guest_uuid = Uuid::new_v4();
    let daemon = TestDaemon::with_config(config_with_guest(guest_uuid))
        .await
        .expect("test daemon");
    let manager = manager_proxy(&daemon).await;

    let err = manager
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "DuplicateSystemProfile",
            &(guest_uuid.to_string(), "guest"),
        )
        .await
        .expect_err("collision must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("InvalidArgs") || msg.contains("already exists"),
        "expected InvalidArgs / duplicate-name error, got: {msg}"
    );
}

#[tokio::test]
async fn duplicate_rejects_unknown_source_uuid() {
    let guest_uuid = Uuid::new_v4();
    let daemon = TestDaemon::with_config(config_with_guest(guest_uuid))
        .await
        .expect("test daemon");
    let manager = manager_proxy(&daemon).await;

    let unknown = Uuid::new_v4();
    let err = manager
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "DuplicateSystemProfile",
            &(unknown.to_string(), "stranger"),
        )
        .await
        .expect_err("unknown source must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("InvalidArgs") || msg.contains("Unknown"),
        "expected InvalidArgs / unknown-uuid error, got: {msg}"
    );
}
