//! `Session1.SetTvUserProfile` accepts profile UUIDs only; unknown
//! UUIDs are rejected with `InvalidArgs`, malformed UUID strings are
//! rejected with `InvalidArgs`, and known UUIDs (including the
//! default UUID, which always resolves even without a body) route
//! through to a normal assignment write.

use std::path::PathBuf;

use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SESSION_SERVICE, UserConfig, UserProfileConfig,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_proxy<'a>(conn: &'a zbus::Connection) -> zbus::Proxy<'a> {
    zbus::proxy::Builder::<zbus::Proxy<'_>>::new(conn)
        .destination(SESSION_SERVICE)
        .expect("destination")
        .path("/org/tisvlay/Session")
        .expect("path")
        .interface("org.tisvlay.Session1")
        .expect("interface")
        .build()
        .await
        .expect("Session1 proxy")
}

fn config_with_default() -> UserConfig {
    let mut config = UserConfig::default();
    config.user.profiles.insert(
        DEFAULT_USER_PROFILE_UUID,
        UserProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..Default::default()
        },
    );
    config
}

#[tokio::test]
async fn set_rejects_unknown_uuid_with_invalid_args() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    config_with_default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("uuid_unknown_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await;

    // Well-formed UUID that doesn't exist in the catalog.
    let absent = Uuid::new_v4().to_string();
    let err = proxy
        .call::<_, _, ()>("SetTvUserProfile", &("uuid_unknown_tv", absent.as_str()))
        .await
        .expect_err("setting an unknown profile UUID should fail");
    let name = err.to_string().to_lowercase();
    assert!(
        name.contains("invalidargs") || name.contains("unknown"),
        "error should surface as InvalidArgs / Unknown user profile; got {err:?}",
    );
}

#[tokio::test]
async fn set_rejects_malformed_uuid_with_invalid_args() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    config_with_default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("uuid_malformed_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await;

    let err = proxy
        .call::<_, _, ()>("SetTvUserProfile", &("uuid_malformed_tv", "not-a-uuid"))
        .await
        .expect_err("setting a malformed UUID should fail");
    let name = err.to_string().to_lowercase();
    assert!(
        name.contains("invalidargs") || name.contains("invalid"),
        "error should surface as InvalidArgs / Invalid UUID; got {err:?}",
    );
}

#[tokio::test]
async fn set_accepts_default_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    config_with_default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("uuid_default_tv", config_path)
        .await
        .expect("test session");
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = session_proxy(&conn).await;

    let default_uuid_str = DEFAULT_USER_PROFILE_UUID.to_string();
    let _: () = proxy
        .call(
            "SetTvUserProfile",
            &("uuid_default_tv", default_uuid_str.as_str()),
        )
        .await
        .expect("SetTvUserProfile with default UUID");

    let returned: String = proxy
        .call("GetTvUserProfile", &("uuid_default_tv",))
        .await
        .expect("GetTvUserProfile");
    assert_eq!(returned, default_uuid_str);
}
