//! `Manager1.DeleteSystemProfile(DEFAULT_SYSTEM_PROFILE_UUID)` is
//! rejected as `AccessDenied`.

#![cfg(feature = "mock")]

use tisvlay_core::{DAEMON_SERVICE, DEFAULT_SYSTEM_PROFILE_UUID};
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn delete_default_rejected_as_access_denied() {
    let daemon = TestDaemon::with_one_mock_tv("defdel_tv")
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
        .call::<_, _, ()>(
            "DeleteSystemProfile",
            &(DEFAULT_SYSTEM_PROFILE_UUID.to_string().as_str(),),
        )
        .await
        .expect_err("deleting the default profile must fail");

    let name = match &err {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_string(),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        name, "org.freedesktop.DBus.Error.AccessDenied",
        "default delete should map to AccessDenied; got {err:?}",
    );
}
