//! `Session1.ListUserProfiles` returns the catalog as a UUID-keyed
//! `a{sa{sv}}` body dict. The reserved-default UUID is always
//! present — backed by the admin baseline when no user-defined body
//! shadows it. User-defined profiles flow through alongside the
//! default.

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
async fn list_includes_default_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("list_default_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let bodies = client.list_user_profile_bodies().await.expect("list");
    assert!(
        bodies.contains_key(&DEFAULT_USER_PROFILE_UUID),
        "reserved default UUID must always be in the catalog; got {bodies:?}",
    );
    let default = &bodies[&DEFAULT_USER_PROFILE_UUID];
    assert_eq!(default.name, DEFAULT_PROFILE_NAME);
}

#[tokio::test]
async fn list_includes_user_defined_profiles() {
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

    let _session = TestSession::with_user_config_path("list_userdef_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let bodies = client.list_user_profile_bodies().await.expect("list");
    assert!(
        bodies.contains_key(&gaming_uuid),
        "user-defined gaming profile must appear in catalog; got {bodies:?}",
    );
    assert_eq!(bodies[&gaming_uuid].name, "gaming");
    // Reserved default still present.
    assert!(bodies.contains_key(&DEFAULT_USER_PROFILE_UUID));
}
