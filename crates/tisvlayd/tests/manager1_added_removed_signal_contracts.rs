//! `Manager1.SystemProfileAdded(uuid)` fires after Create + Duplicate;
//! `Manager1.SystemProfileRemoved(uuid)` fires after Delete. Both
//! signals carry the affected UUID, and clients are guaranteed to be
//! able to fetch / not fetch the body the moment the signal arrives —
//! emission is gated by successful persistence.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{DaemonClient, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn added_fires_on_create_and_duplicate_removed_fires_on_delete() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    // Subscribe BEFORE any mutation.
    let added = client.system_profile_added_stream().await.expect("added");
    let removed = client
        .system_profile_removed_stream()
        .await
        .expect("removed");
    tokio::pin!(added);
    tokio::pin!(removed);

    // ---- Create → Added(uuid) ------------------------------------
    let created = client.create_system_profile("alpha").await.unwrap();
    let signal_uuid = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("Added signal must arrive after Create")
        .expect("stream must yield");
    assert_eq!(signal_uuid, created, "Added payload matches Create return");

    // ---- Delete → Removed(uuid) ----------------------------------
    client.delete_system_profile(created).await.unwrap();
    let signal_uuid = tokio::time::timeout(Duration::from_secs(2), removed.next())
        .await
        .expect("Removed signal must arrive after Delete")
        .expect("stream must yield");
    assert_eq!(signal_uuid, created, "Removed payload matches deleted UUID");

    // ---- Duplicate → Added(new_uuid) -----------------------------
    let source = client.create_system_profile("beta").await.unwrap();
    // Drain the Added we just emitted for the new source.
    let added_for_source = tokio::time::timeout(Duration::from_secs(2), added.next())
        .await
        .expect("Added signal after Create")
        .expect("stream yields");
    assert_eq!(added_for_source, source);

    let duplicated = client
        .duplicate_system_profile(source, "beta-copy")
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
