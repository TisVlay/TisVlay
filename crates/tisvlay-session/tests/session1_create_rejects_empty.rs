//! `Session1.CreateUserProfile("")` and whitespace-only names are
//! rejected as `InvalidArgs`.

use tempfile::TempDir;
use tisvlay_harness::session::TestSession;

async fn expect_invalid_args(_session: &TestSession, name: &str) {
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

    let err: zbus::Error = proxy
        .call::<_, _, zbus::zvariant::OwnedObjectPath>("CreateUserProfile", &(name,))
        .await
        .expect_err("creating with empty/whitespace name must fail");

    let err_name = match &err {
        zbus::Error::MethodError(n, _, _) => n.as_str().to_string(),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        err_name, "org.freedesktop.DBus.Error.InvalidArgs",
        "empty/whitespace user-profile name should map to InvalidArgs; got {err:?}",
    );
}

#[tokio::test]
async fn empty_name_rejected() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let session = TestSession::with_user_config_path("empty_user_tv", cfg)
        .await
        .unwrap();
    expect_invalid_args(&session, "").await;
}

#[tokio::test]
async fn whitespace_name_rejected() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let session = TestSession::with_user_config_path("ws_user_tv", cfg)
        .await
        .unwrap();
    expect_invalid_args(&session, "   ").await;
}
