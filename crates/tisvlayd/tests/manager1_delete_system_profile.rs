//! `Manager1.DeleteSystemProfile(uuid)` unregisters the per-profile
//! interface, fires `InterfacesRemoved`, and clears the profile from
//! `GetManagedObjects`.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::DAEMON_SERVICE;
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn delete_removes_profile_object() {
    let daemon = TestDaemon::with_one_mock_tv("del_tv")
        .await
        .expect("test daemon");

    let manager = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(daemon.connection())
        .destination(DAEMON_SERVICE)
        .unwrap()
        .path("/org/tisvlay/Manager")
        .unwrap()
        .interface("org.tisvlay.Manager1")
        .unwrap()
        .build()
        .await
        .unwrap();

    // Subscribe to InterfacesRemoved before issuing the delete so we
    // can't race the signal emission with the proxy build.
    let bus = tisvlay_test::PrivateBus::shared();
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

    let new_path: zbus::zvariant::OwnedObjectPath = manager
        .call("CreateSystemProfile", &("doomed",))
        .await
        .expect("create");

    let path_str = new_path.as_str().to_string();
    let uuid = path_str
        .rsplit('/')
        .next()
        .unwrap()
        .replace('_', "-")
        .to_string();

    let _: () = manager
        .call("DeleteSystemProfile", &(uuid.as_str(),))
        .await
        .expect("delete");

    // InterfacesRemoved should arrive within a generous test budget.
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

    // GetManagedObjects no longer surfaces the path.
    let object_manager = zbus::fdo::ObjectManagerProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .unwrap()
        .path("/org/tisvlay")
        .unwrap()
        .build()
        .await
        .unwrap();
    let managed = object_manager.get_managed_objects().await.unwrap();
    assert!(
        managed.keys().all(|p| p.as_str() != path_str),
        "deleted profile should not appear in GetManagedObjects",
    );
}
