//! `Session1.DuplicateUserProfile(source_uuid, new_name)` clones a
//! source profile under a new UUID + name. The new body carries every
//! field from the source except `name`. Absent source UUIDs reject
//! with `ProfileNotFound`; reserved / empty / duplicate names reject
//! with `ProfileNameInvalid`.

use std::path::PathBuf;

use tisvlay_core::{SessionAction, SessionClient, UserConfig, UserProfileConfig};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn duplicate_clones_fields_under_new_name() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let gaming_uuid = Uuid::new_v4();
    config.user.profiles.insert(
        gaming_uuid,
        UserProfileConfig {
            name: "gaming".to_string(),
            on_session_lock: Some(SessionAction::None),
            idle_timeout_seconds: Some(0),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("duplicate_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let new_uuid = client
        .duplicate_user_profile(gaming_uuid, "gaming-copy")
        .await
        .expect("duplicate");
    assert_ne!(new_uuid, gaming_uuid);

    let cloned = client.get_user_profile(new_uuid).await.unwrap();
    assert_eq!(cloned.name, "gaming-copy");
    assert_eq!(cloned.on_session_lock, Some(SessionAction::None));
    assert_eq!(cloned.idle_timeout_seconds, Some(0));
}

#[tokio::test]
async fn duplicate_rejects_absent_source_with_profile_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("duplicate_absent_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let absent = Uuid::new_v4();
    let err = client
        .duplicate_user_profile(absent, "any-name")
        .await
        .expect_err("absent source must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound for absent source; got: {chain}",
    );
}

#[tokio::test]
async fn duplicate_rejects_duplicate_new_name() {
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

    let _session = TestSession::with_user_config_path("duplicate_dup_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let err = client
        .duplicate_user_profile(gaming_uuid, "gaming")
        .await
        .expect_err("name collision must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("already in use"),
        "expected ProfileNameInvalid for collision; got: {chain}",
    );
}
