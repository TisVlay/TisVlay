//! `Manager1.CreateSystemProfile` rejects a second call with the
//! same name as `InvalidArgs` so the GUI's master list can route on
//! the standard error name.

#![cfg(feature = "mock")]

use tisvlay_core::DAEMON_SERVICE;
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn duplicate_name_rejected_as_invalid_args() {
    let daemon = TestDaemon::with_one_mock_tv("dup_tv")
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

    let _: zbus::zvariant::OwnedObjectPath = manager
        .call("CreateSystemProfile", &("guest",))
        .await
        .expect("first CreateSystemProfile");

    let err: zbus::Error = manager
        .call::<_, _, zbus::zvariant::OwnedObjectPath>("CreateSystemProfile", &("guest",))
        .await
        .expect_err("second CreateSystemProfile must fail");

    let name = match &err {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_string(),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        name, "org.freedesktop.DBus.Error.InvalidArgs",
        "duplicate name should map to InvalidArgs; got {err:?}",
    );
}
