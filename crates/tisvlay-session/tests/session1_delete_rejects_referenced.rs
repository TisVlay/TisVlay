//! `Session1.DeleteUserProfile` is rejected with `AccessDenied`
//! when a TV's `profile.user` references the profile. The error
//! message names the offending TV UUID.

use tempfile::TempDir;
use tisvlay_harness::daemon::test_tv_uuid;
use tisvlay_harness::session::TestSession;

#[tokio::test]
async fn referenced_user_profile_cannot_be_deleted() {
    let dir = TempDir::new().unwrap();
    let cfg = dir.path().join("user-config.toml");
    let _session = TestSession::with_user_config_path("user_blockdel_tv", cfg)
        .await
        .unwrap();
    let tv_uuid = test_tv_uuid("user_blockdel_tv").to_string();

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

    let new_path: zbus::zvariant::OwnedObjectPath = proxy
        .call("CreateUserProfile", &("movies",))
        .await
        .expect("create");
    let path_str = new_path.as_str().to_string();
    let profile_uuid = path_str
        .rsplit('/')
        .next()
        .unwrap()
        .replace('_', "-")
        .to_string();

    let _: () = proxy
        .call(
            "SetTvUserProfile",
            &(tv_uuid.as_str(), profile_uuid.as_str()),
        )
        .await
        .expect("set TV user profile");

    let err: zbus::Error = proxy
        .call::<_, _, ()>("DeleteUserProfile", &(profile_uuid.as_str(),))
        .await
        .expect_err("referenced user profile must not delete");

    let (name, body) = match &err {
        zbus::Error::MethodError(name, body, _) => (name.as_str().to_string(), body.clone()),
        other => panic!("expected MethodError, got {other:?}"),
    };
    assert_eq!(
        name, "org.freedesktop.DBus.Error.AccessDenied",
        "referenced user-profile delete should map to AccessDenied; got {err:?}",
    );
    let body = body.unwrap_or_default();
    assert!(
        body.contains(&tv_uuid),
        "error body should name TV UUID {tv_uuid}; got {body:?}",
    );
}
