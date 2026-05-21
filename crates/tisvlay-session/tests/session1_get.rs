//! `Session1.GetUserProfile(uuid)` returns the body for one UUID.
//! Known UUIDs return their bodies (including the reserved default,
//! which falls through to the admin baseline when no user body
//! shadows it); absent UUIDs return `ProfileNotFound`; malformed
//! UUIDs also route through `ProfileNotFound` for uniform error
//! handling.

use std::path::PathBuf;

use tisvlay_core::{
    DEFAULT_PROFILE_NAME, DEFAULT_USER_PROFILE_UUID, SessionClient, UserConfig, UserProfileConfig,
};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn get_returns_default_body() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("get_default_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let body = client
        .get_user_profile(DEFAULT_USER_PROFILE_UUID)
        .await
        .expect("default profile must always be fetchable");
    assert_eq!(body.name, DEFAULT_PROFILE_NAME);
}

#[tokio::test]
async fn get_returns_user_defined_body() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let gaming_uuid = Uuid::new_v4();
    config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("get_userdef_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let body = client
        .get_user_profile(gaming_uuid)
        .await
        .expect("user-defined profile must be fetchable");
    assert_eq!(body.name, "gaming");
}

#[tokio::test]
async fn get_rejects_absent_uuid_with_profile_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("get_absent_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let absent = Uuid::new_v4();
    let err = client
        .get_user_profile(absent)
        .await
        .expect_err("absent UUID must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound for absent UUID; got: {chain}",
    );
}

#[tokio::test]
async fn get_rejects_malformed_uuid_with_profile_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("get_malformed_tv", config_path)
        .await
        .expect("test session");

    // Bypass the typed wrapper (which only takes a `Uuid`) to send a
    // malformed string straight at the wire.
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Session",
        "/org/tisvlay/Session",
        "org.tisvlay.Session1",
    )
    .await
    .expect("proxy");

    let result = proxy.call_method("GetUserProfile", &("not-a-uuid",)).await;
    let err = result.expect_err("malformed UUID must reject");
    let chain = err.to_string().to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound for malformed UUID; got: {err:?}",
    );
}
