//! `Session1.DuplicateUserProfile(source_uuid, new_name)` clones a
//! user profile's locally-set fields under a fresh UUID + new name.

use tempfile::TempDir;
use tisvlay_harness::session::TestSession;

#[tokio::test]
async fn duplicate_preserves_locally_set_field() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("dup_user_tv", cfg)
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

    // Seed: create "evening" then locally-set OnSessionLock = screen_off.
    let source_path: zbus::zvariant::OwnedObjectPath = proxy
        .call("CreateUserProfile", &("evening",))
        .await
        .expect("CreateUserProfile");
    let source_uuid = source_path
        .as_str()
        .strip_prefix("/org/tisvlay/UserProfile/")
        .unwrap()
        .replace('_', "-");

    let source_props = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination("org.tisvlay.Session")
        .unwrap()
        .path(source_path.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    source_props
        .set(
            "org.tisvlay.UserProfile1".try_into().unwrap(),
            "OnSessionLock",
            zbus::zvariant::Value::from("screen_off"),
        )
        .await
        .expect("Set OnSessionLock");

    // Duplicate.
    let new_path: zbus::zvariant::OwnedObjectPath = proxy
        .call("DuplicateUserProfile", &(source_uuid.as_str(), "evening-2"))
        .await
        .expect("DuplicateUserProfile");

    assert_ne!(new_path.as_str(), source_path.as_str(), "fresh UUID");

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
        .expect("duplicate present in GetManagedObjects");
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
    assert_eq!(name, "evening-2");

    let locally_set: Vec<String> = props
        .get("LocallySetFields")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        locally_set,
        vec!["on_session_lock".to_string()],
        "copy carries the same locally-set fields as the source"
    );

    let on_session_lock: String = props
        .get("OnSessionLock")
        .unwrap()
        .try_to_owned()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(on_session_lock, "screen_off");

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
async fn duplicate_rejects_unknown_source_uuid() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("dup_user_tv_err", cfg)
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

    let unknown = uuid::Uuid::new_v4().to_string();
    let err = proxy
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "DuplicateUserProfile",
            &(unknown.as_str(), "stranger"),
        )
        .await
        .expect_err("unknown source must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("InvalidArgs") || msg.contains("Unknown"),
        "expected InvalidArgs / unknown-uuid error, got: {msg}"
    );
}
