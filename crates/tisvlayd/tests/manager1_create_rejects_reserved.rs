//! `Manager1.CreateSystemProfile("default")` is rejected — the
//! reserved name belongs to the default UUID.

#![cfg(feature = "mock")]

use tisvlay_core::{DAEMON_SERVICE, DEFAULT_PROFILE_NAME};
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn reserved_name_rejected_as_invalid_args() {
    let daemon = TestDaemon::with_one_mock_tv("resv_tv")
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

    let err: zbus::Error = manager
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "CreateSystemProfile",
            &(DEFAULT_PROFILE_NAME,),
        )
        .await
        .expect_err("creating the reserved 'default' name must fail");

    let name = match &err {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_string(),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        name, "org.freedesktop.DBus.Error.InvalidArgs",
        "reserved name should map to InvalidArgs; got {err:?}",
    );
}
