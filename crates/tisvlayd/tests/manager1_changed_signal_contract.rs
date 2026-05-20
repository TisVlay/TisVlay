//! `Manager1.SystemProfileChanged(uuid)` fires exactly once per
//! successful `UpdateSystemProfile` and carries the UUID of the
//! mutated body. The signal is the client's cue to drop any cached
//! body for the named UUID.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{DaemonClient, SessionAction, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn changed_fires_once_with_matching_uuid() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let uuid = client.create_system_profile("watched").await.unwrap();

    // Subscribe BEFORE the mutation. Signals emitted before the
    // subscription is set up would be dropped.
    let stream = client
        .system_profile_changed_stream()
        .await
        .expect("stream");
    tokio::pin!(stream);

    // Fire one mutation.
    let mut body = client.get_system_profile(uuid).await.unwrap();
    body.on_shutdown = Some(SessionAction::PowerOff);
    client.update_system_profile(uuid, &body).await.unwrap();

    // Drain the first event with a generous timeout — the signal is
    // emitted right after persistence, so it should arrive promptly.
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("signal must arrive within timeout")
        .expect("stream must yield a UUID");
    assert_eq!(first, uuid, "signal payload must match the mutated UUID");

    // No further events should arrive — only one mutation happened.
    let extra = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(
        extra.is_err(),
        "exactly one SystemProfileChanged per UpdateSystemProfile; got extra: {extra:?}",
    );
}
