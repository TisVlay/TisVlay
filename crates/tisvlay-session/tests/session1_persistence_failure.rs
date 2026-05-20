//! Persistence-failure rollback: when `UpdateUserProfile` cannot write
//! its mutation to disk, the in-memory snapshot must roll back to the
//! pre-mutation body and the call must surface
//! `org.tisvlay.Session1.Error.IoError`. No `UserProfileBodyChanged`
//! signal must fire — the contract is "fetchable by the moment the
//! signal arrives", so a signal without a persisted change would lie
//! to subscribers.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{SessionAction, SessionClient, UserConfig, UserProfileConfig};
use tisvlay_harness::session::TestSession;
use uuid::Uuid;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn update_rolls_back_in_memory_on_persist_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let target = Uuid::new_v4();
    config.user.profiles.insert(
        target,
        UserProfileConfig {
            name: "rollback".to_string(),
            on_session_lock: Some(SessionAction::ScreenOff),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("rollback_tv", config_path.clone())
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    let pre_mutation = client.get_user_profile(target).await.unwrap();

    // Subscribe to UserProfileBodyChanged BEFORE the failing mutation
    // so we can drain afterwards and confirm no signal fires.
    let changed_stream = client
        .user_profile_body_changed_stream()
        .await
        .expect("stream");
    tokio::pin!(changed_stream);

    // Drop write permission on the config FILE itself so the session's
    // save_to() -> std::fs::write fails on the open-with-truncate step.
    // (Setting only the directory to read-only doesn't block writes to
    // existing files — Linux file ownership perms govern that path.)
    let original_perms = std::fs::metadata(&config_path)
        .expect("metadata")
        .permissions();
    let mut readonly = original_perms.clone();
    readonly.set_mode(0o400);
    std::fs::set_permissions(&config_path, readonly).expect("chmod file readonly");

    let mut mutated = pre_mutation.clone();
    mutated.on_idle_timeout = Some(SessionAction::PowerOff);
    let err = client
        .update_user_profile(target, &mutated)
        .await
        .expect_err("persist must fail under readonly config file");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("ioerror") || chain.contains("io error") || chain.contains("failed to save"),
        "expected IoError on persist failure; got: {chain}",
    );

    // Restore file permissions so subsequent reads / tempdir cleanup
    // work normally.
    std::fs::set_permissions(&config_path, original_perms).expect("restore perms");

    // Refetch confirms in-memory rollback.
    let after = client.get_user_profile(target).await.expect("refetch");
    assert_eq!(
        after.on_idle_timeout, pre_mutation.on_idle_timeout,
        "on_idle_timeout must roll back",
    );
    assert_eq!(
        after.on_session_lock, pre_mutation.on_session_lock,
        "unrelated fields must remain untouched",
    );

    // No signal should arrive.
    let drain = tokio::time::timeout(Duration::from_millis(100), changed_stream.next()).await;
    assert!(
        drain.is_err(),
        "no UserProfileBodyChanged signal should fire on failed update; got: {drain:?}",
    );
}
