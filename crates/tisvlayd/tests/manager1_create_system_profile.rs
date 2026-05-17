//! `Manager1.CreateSystemProfile(name)` creates a fresh profile,
//! registers the `SystemProfile1` interface, returns the new path,
//! and surfaces the profile via `ObjectManager.GetManagedObjects`
//! with `Name` set + empty `LocallySetFields` / `ReferencedBy`.

#![cfg(feature = "mock")]

use tisvlay_core::DAEMON_SERVICE;
use tisvlay_harness::daemon::TestDaemon;

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

#[tokio::test]
async fn create_system_profile_round_trips() {
    let daemon = TestDaemon::with_one_mock_tv("guest_tv")
        .await
        .expect("test daemon");

    let manager = manager_proxy(&daemon).await;
    let new_path: zbus::zvariant::OwnedObjectPath = manager
        .call("CreateSystemProfile", &("guest",))
        .await
        .expect("CreateSystemProfile");

    // Path lives at /org/tisvlay/SystemProfile/<UUID_>.
    assert!(
        new_path.as_str().starts_with("/org/tisvlay/SystemProfile/"),
        "expected SystemProfile path, got {}",
        new_path.as_str()
    );

    // ObjectManager surfaces the new profile.
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
    assert_eq!(name, "guest");

    let locally_set: Vec<String> = props
        .get("LocallySetFields")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert!(
        locally_set.is_empty(),
        "fresh profile inherits everything; got {locally_set:?}",
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
        "fresh profile has no TV references; got {referenced_by:?}",
    );
}
