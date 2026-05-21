//! `Session1.UserProfileAdded(uuid)` fires after Create + Duplicate;
//! `Session1.UserProfileRemoved(uuid)` fires after Delete. Both
//! signals carry the affected UUID, and clients are guaranteed to be
//! able to fetch / not fetch the body the moment the signal arrives —
//! emission is gated by successful persistence.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{SessionClient, UserConfig};
use tisvlay_harness::session::TestSession;

async fn session_client_on_test_bus() -> SessionClient {
    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    SessionClient::from_connection(conn).await.unwrap()
}

#[tokio::test]
async fn added_fires_on_create_and_duplicate_removed_fires_on_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("added_removed_tv", config_path)
        .await
        .expect("test session");

    let client = session_client_on_test_bus().await;
    // Subscribe BEFORE any mutation.
    let added = client.user_profile_added_stream().await.expect("added");
    let removed = client.user_profile_removed_stream().await.expect("removed");
    tokio::pin!(added);
    tokio::pin!(removed);

    // ---- Create → Added(uuid) ------------------------------------
    let created = client.create_user_profile("alpha").await.unwrap();
    let signal_uuid = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("Added signal must arrive after Create")
        .expect("stream must yield");
    assert_eq!(signal_uuid, created, "Added payload matches Create return");

    // ---- Delete → Removed(uuid) ----------------------------------
    client.delete_user_profile(created).await.unwrap();
    let signal_uuid = tokio::time::timeout(Duration::from_secs(2), removed.next())
        .await
        .expect("Removed signal must arrive after Delete")
        .expect("stream must yield");
    assert_eq!(signal_uuid, created, "Removed payload matches deleted UUID");

    // ---- Duplicate → Added(new_uuid) -----------------------------
    let source = client.create_user_profile("beta").await.unwrap();
    let added_for_source = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("Added signal after Create")
        .expect("stream yields");
    assert_eq!(added_for_source, source);

    let duplicated = client
        .duplicate_user_profile(source, "beta-copy")
        .await
        .unwrap();
    let added_for_dup = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("Added signal after Duplicate")
        .expect("stream yields");
    assert_eq!(
        added_for_dup, duplicated,
        "Added payload matches Duplicate return",
    );

    // No spurious Removed events from Duplicate.
    let no_remove = tokio::time::timeout(Duration::from_millis(100), removed.next()).await;
    assert!(
        no_remove.is_err(),
        "Duplicate must not emit Removed; got: {no_remove:?}",
    );
}
