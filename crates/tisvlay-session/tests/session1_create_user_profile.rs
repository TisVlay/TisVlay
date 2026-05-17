//! `Session1.CreateUserProfile(name)` creates a fresh user profile,
//! registers the `UserProfile1` interface on the user bus, returns
//! the new path, and surfaces the profile via
//! `ObjectManager.GetManagedObjects` with `Name` set + empty
//! `LocallySetFields` / `ReferencedBy`.

use tempfile::TempDir;
use tisvlay_harness::session::TestSession;

#[tokio::test]
async fn create_user_profile_round_trips() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("create_user_tv", cfg)
        .await
        .unwrap();

    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();

    let proxy = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&conn)
        .destination("org.tisvlay.Session")
        .unwrap()
        .path("/org/tisvlay/Session")
        .unwrap()
        .interface("org.tisvlay.Session1")
        .unwrap()
        .build()
        .await
        .unwrap();
    let new_path: zbus::zvariant::OwnedObjectPath = proxy
        .call("CreateUserProfile", &("evening",))
        .await
        .expect("CreateUserProfile");

    assert!(
        new_path.as_str().starts_with("/org/tisvlay/UserProfile/"),
        "expected UserProfile path, got {}",
        new_path.as_str(),
    );

    let object_manager = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.tisvlay.Session")
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
        .get("org.tisvlay.UserProfile1")
        .expect("UserProfile1 interface present");
    let name: String = props
        .get("Name")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(name, "evening");
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
