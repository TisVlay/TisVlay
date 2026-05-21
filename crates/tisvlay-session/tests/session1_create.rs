//! `Session1.CreateUserProfile(name)` mints a fresh UUID, inserts a
//! minimal body (only `name` set), persists, and emits
//! `UserProfileAdded(uuid)`. Empty / whitespace / reserved `"default"`
//! / duplicate-of-existing-name all reject with
//! `org.tisvlay.Session1.Error.ProfileNameInvalid`.

use std::path::PathBuf;

use tisvlay_core::{SessionClient, UserConfig};
use tisvlay_harness::session::TestSession;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn create_returns_uuid_and_minimal_body() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("create_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let new_uuid = client
        .create_user_profile("guest")
        .await
        .expect("create guest");

    let body = client.get_user_profile(new_uuid).await.expect("get body");
    assert_eq!(body.name, "guest");
    assert!(body.on_session_lock.is_none());
    assert!(body.on_session_unlock.is_none());
    assert!(body.on_greeter_wake.is_none());
    assert!(body.on_idle_timeout.is_none());
    assert!(body.on_idle_resume.is_none());
    assert!(body.idle_timeout_seconds.is_none());

    let bodies = client.list_user_profile_bodies().await.unwrap();
    assert!(
        bodies.contains_key(&new_uuid),
        "newly-created profile must appear in catalog; got {bodies:?}",
    );
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("create_empty_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let err = client
        .create_user_profile("")
        .await
        .expect_err("empty name must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("non-empty"),
        "expected ProfileNameInvalid for empty name; got: {chain}",
    );
}

#[tokio::test]
async fn create_rejects_whitespace_only_name() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("create_ws_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let err = client
        .create_user_profile("   ")
        .await
        .expect_err("whitespace-only name must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("non-empty"),
        "expected ProfileNameInvalid for whitespace-only name; got: {chain}",
    );
}

#[tokio::test]
async fn create_rejects_reserved_default_literal() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("create_default_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let err = client
        .create_user_profile("default")
        .await
        .expect_err("reserved literal must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("reserved"),
        "expected ProfileNameInvalid for 'default'; got: {chain}",
    );
}

#[tokio::test]
async fn create_rejects_duplicate_of_existing_name() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("create_dup_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    client
        .create_user_profile("studio")
        .await
        .expect("first create");

    let err = client
        .create_user_profile("studio")
        .await
        .expect_err("duplicate name must reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("already in use"),
        "expected ProfileNameInvalid for duplicate name; got: {chain}",
    );
}
