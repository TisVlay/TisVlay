//! `Session1.DeleteUserProfile(DEFAULT_USER_PROFILE_UUID)` is
//! rejected with `AccessDenied`.

use tempfile::TempDir;
use tisvlay_core::DEFAULT_USER_PROFILE_UUID;
use tisvlay_harness::session::TestSession;

#[tokio::test]
async fn delete_default_user_profile_rejected_as_access_denied() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("defdel_user_tv", cfg)
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
        .call::<_, _, ()>(
            "DeleteUserProfile",
            &(DEFAULT_USER_PROFILE_UUID.to_string().as_str(),),
        )
        .await
        .expect_err("deleting the default user profile must fail");

    let name = match &err {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_string(),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        name, "org.freedesktop.DBus.Error.AccessDenied",
        "default user-profile delete should map to AccessDenied; got {err:?}",
    );
}
