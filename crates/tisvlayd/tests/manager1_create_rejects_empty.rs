//! `Manager1.CreateSystemProfile("")` and whitespace-only names are
//! rejected as `InvalidArgs`.

#![cfg(feature = "mock")]

use tisvlay_core::DAEMON_SERVICE;
use tisvlay_harness::daemon::TestDaemon;

async fn expect_invalid_args(daemon: &TestDaemon, name: &str) {
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
        .call::<_, _, zbus::zvariant::OwnedObjectPath>("CreateSystemProfile", &(name,))
        .await
        .expect_err("creating with empty name must fail");

    let err_name = match &err {
        zbus::Error::MethodError(n, _, _) => n.as_str().to_string(),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        err_name, "org.freedesktop.DBus.Error.InvalidArgs",
        "empty/whitespace name should map to InvalidArgs; got {err:?}",
    );
}

#[tokio::test]
async fn empty_name_rejected() {
    let daemon = TestDaemon::with_one_mock_tv("empty_tv")
        .await
        .expect("test daemon");
    expect_invalid_args(&daemon, "").await;
}

#[tokio::test]
async fn whitespace_name_rejected() {
    let daemon = TestDaemon::with_one_mock_tv("ws_tv")
        .await
        .expect("test daemon");
    expect_invalid_args(&daemon, "   ").await;
}
