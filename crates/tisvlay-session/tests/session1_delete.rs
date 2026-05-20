//! `Session1.DeleteUserProfile(uuid)` removes a profile from the
//! catalog and persists. Rejects the reserved-default UUID with
//! `ReservedProfile`. Absent UUIDs surface as `ProfileNotFound`.

use std::path::PathBuf;

use tisvlay_core::{DEFAULT_USER_PROFILE_UUID, SessionClient, UserConfig, UserProfileConfig};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn delete_removes_profile_and_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let guest_uuid = Uuid::new_v4();
    config.user.profiles.insert(
        guest_uuid,
        UserProfileConfig {
            name: "guest".to_string(),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("delete_tv", config_path.clone())
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    client
        .delete_user_profile(guest_uuid)
        .await
        .expect("delete");

    let bodies = client.list_user_profile_bodies().await.unwrap();
    assert!(
        !bodies.contains_key(&guest_uuid),
        "deleted profile must be gone from catalog; got {bodies:?}",
    );

    // On-disk reflects the removal.
    let on_disk = UserConfig::load_from(&config_path)
        .expect("load")
        .expect("file exists");
    assert!(
        !on_disk.user.profiles.contains_key(&guest_uuid),
        "deleted profile must not appear in saved catalog",
    );
}

#[tokio::test]
async fn delete_rejects_default_uuid_with_reserved_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("delete_default_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let err = client
        .delete_user_profile(DEFAULT_USER_PROFILE_UUID)
        .await
        .expect_err("default UUID must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("reservedprofile") || chain.contains("reserved"),
        "expected ReservedProfile for default UUID; got: {chain}",
    );
}

#[tokio::test]
async fn delete_rejects_absent_uuid_with_profile_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("delete_absent_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let absent = Uuid::new_v4();
    let err = client
        .delete_user_profile(absent)
        .await
        .expect_err("absent UUID must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound for absent UUID; got: {chain}",
    );
}
