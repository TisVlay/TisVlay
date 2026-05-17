//! `Session1.DeleteUserProfile(uuid)` unregisters the per-profile
//! interface, fires `InterfacesRemoved` on the user bus, and clears
//! the profile from `GetManagedObjects`.

use std::time::Duration;

use futures_util::StreamExt;
use tempfile::TempDir;
use tisvlay_harness::session::TestSession;

#[tokio::test]
async fn delete_removes_user_profile_object() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("del_user_tv", cfg)
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

    // Subscribe to InterfacesRemoved before the delete.
    let watcher = bus.connect().await.unwrap();
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.ObjectManager")
        .unwrap()
        .member("InterfacesRemoved")
        .unwrap()
        .build();
    let stream = zbus::MessageStream::for_match_rule(rule, &watcher, None)
        .await
        .unwrap();

    let new_path: zbus::zvariant::OwnedObjectPath = proxy
        .call("CreateUserProfile", &("doomed",))
        .await
        .expect("create");
    let path_str = new_path.as_str().to_string();
    let uuid = path_str
        .rsplit('/')
        .next()
        .unwrap()
        .replace('_', "-")
        .to_string();

    let _: () = proxy
        .call("DeleteUserProfile", &(uuid.as_str(),))
        .await
        .expect("delete");

    let mut stream = std::pin::pin!(stream);
    let mut saw_remove = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(Ok(msg))) => {
                if let Ok((removed_path, _ifaces)) = msg
                    .body()
                    .deserialize::<(zbus::zvariant::OwnedObjectPath, Vec<String>)>()
                {
                    if removed_path.as_str() == path_str {
                        saw_remove = true;
                        break;
                    }
                }
            }
            Ok(Some(Err(_))) => continue,
            Ok(None) | Err(_) => continue,
        }
    }
    assert!(saw_remove, "InterfacesRemoved should fire for {path_str}");

    let object_manager = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.tisvlay.Session")
        .unwrap()
        .path("/org/tisvlay")
        .unwrap()
        .build()
        .await
        .unwrap();
    let managed = object_manager.get_managed_objects().await.unwrap();
    assert!(
        managed.keys().all(|p| p.as_str() != path_str),
        "deleted user profile should not appear in GetManagedObjects",
    );
}
