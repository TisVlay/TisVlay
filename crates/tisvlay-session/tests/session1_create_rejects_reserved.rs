//! `Session1.CreateUserProfile("default")` is rejected with
//! `InvalidArgs` — the reserved name belongs to the default UUID.

use tempfile::TempDir;
use tisvlay_core::DEFAULT_PROFILE_NAME;
use tisvlay_harness::session::TestSession;

#[tokio::test]
async fn reserved_name_rejected_as_invalid_args() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("resv_user_tv", cfg)
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

    let err: zbus::Error = proxy
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "CreateUserProfile",
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
        "reserved user-profile name should map to InvalidArgs; got {err:?}",
    );
}
