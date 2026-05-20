//! `Session1.UpdateUserProfile(uuid, body)` atomically replaces a
//! profile body. The on-disk catalog reflects the change; the
//! in-memory state agrees; absent UUIDs reject with
//! `ProfileNotFound`.

use std::path::PathBuf;

use tisvlay_core::{SessionAction, SessionClient, UserConfig, UserProfileBody, UserProfileConfig};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn update_replaces_body_and_persists() {
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

    let _session = TestSession::with_user_config_path("update_tv", config_path.clone())
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let mut body = client.get_user_profile(gaming_uuid).await.unwrap();
    body.on_idle_timeout = Some(SessionAction::ScreenOff);
    body.idle_timeout_seconds = Some(600);
    client
        .update_user_profile(gaming_uuid, &body)
        .await
        .expect("update");

    let refetched = client.get_user_profile(gaming_uuid).await.unwrap();
    assert_eq!(refetched.on_idle_timeout, Some(SessionAction::ScreenOff));
    assert_eq!(refetched.idle_timeout_seconds, Some(600));

    // On-disk reflects the change.
    let on_disk = UserConfig::load_from(&config_path)
        .expect("load")
        .expect("file exists");
    let stored = on_disk
        .user
        .profiles
        .get(&gaming_uuid)
        .expect("gaming present in saved catalog");
    assert_eq!(stored.on_idle_timeout, Some(SessionAction::ScreenOff));
    assert_eq!(stored.idle_timeout_seconds, Some(600));
}

#[tokio::test]
async fn update_rejects_absent_uuid_with_profile_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("update_absent_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let absent = Uuid::new_v4();
    let body = UserProfileBody {
        name: "anything".to_string(),
        ..Default::default()
    };
    let err = client
        .update_user_profile(absent, &body)
        .await
        .expect_err("absent UUID must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound for absent UUID; got: {chain}",
    );
}

#[tokio::test]
async fn update_rejects_empty_name() {
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

    let _session = TestSession::with_user_config_path("update_empty_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let bad_body = UserProfileBody {
        name: "".to_string(),
        ..Default::default()
    };
    let err = client
        .update_user_profile(gaming_uuid, &bad_body)
        .await
        .expect_err("empty name must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("non-empty"),
        "expected ProfileNameInvalid for empty name; got: {chain}",
    );
}
