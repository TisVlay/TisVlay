//! `Session1.UserProfileBodyChanged(uuid)` fires exactly once per
//! successful `UpdateUserProfile` and carries the UUID of the mutated
//! body. The signal is the client's cue to drop any cached body for
//! the named UUID.
//!
//! Distinct from the pre-existing `Session1.UserProfileChanged(tv_id,
//! profile_uuid)` signal, which fires on per-TV assignment writes via
//! `SetTvUserProfile`. Two signals with different responsibilities;
//! this test pins the body-mutation one.

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
async fn body_changed_fires_once_with_matching_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");

    let mut config = UserConfig::default();
    let target = Uuid::new_v4();
    config.user.profiles.insert(
        target,
        UserProfileConfig {
            name: "watched".to_string(),
            ..Default::default()
        },
    );
    config.save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("body_signal_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    // Subscribe BEFORE the mutation; signals emitted before the
    // subscription is set up would be dropped.
    let stream = client
        .user_profile_body_changed_stream()
        .await
        .expect("stream");
    tokio::pin!(stream);

    let mut body = client.get_user_profile(target).await.unwrap();
    body.on_idle_resume = Some(SessionAction::ScreenOn);
    client.update_user_profile(target, &body).await.unwrap();

    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("signal must arrive within timeout")
        .expect("stream must yield a UUID");
    assert_eq!(first, target, "signal payload must match the mutated UUID");

    // No further events; only one mutation happened.
    let extra = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(
        extra.is_err(),
        "exactly one UserProfileBodyChanged per UpdateUserProfile; got extra: {extra:?}",
    );
}
