//! Persistence-failure rollback: when `UpdateSystemProfile` cannot
//! write its mutation to disk, the in-memory snapshot must roll back
//! to the pre-mutation body and the call must surface
//! `org.tisvlay.Error.IoError`. No `SystemProfileChanged` signal must
//! fire — the contract is "fetchable by the moment the signal arrives",
//! so a signal without a persisted change would lie to subscribers.

#![cfg(feature = "mock")]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use futures_util::StreamExt;
use tisvlay_core::{DaemonClient, SessionAction, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;

#[tokio::test]
async fn update_rolls_back_in_memory_on_persist_failure() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    // Create a profile, populate a field, so we have a known pre-mutation
    // body to roll back to.
    let created = client.create_system_profile("rollback").await.unwrap();
    let mut body = client.get_system_profile(created).await.unwrap();
    body.on_startup = Some(SessionAction::PowerOn);
    client.update_system_profile(created, &body).await.unwrap();

    let pre_mutation = client.get_system_profile(created).await.unwrap();

    // Subscribe to SystemProfileChanged BEFORE making the failing
    // mutation, so we can drain afterwards and confirm no signal fires.
    let changed_stream = client
        .system_profile_changed_stream()
        .await
        .expect("stream");
    tokio::pin!(changed_stream);

    // Drop write permission on the config FILE itself so the daemon's
    // persist_full_config -> save_to(path) -> std::fs::write fails on
    // the open-with-truncate step. (Setting only the directory to
    // read-only doesn't block writes to existing files — Linux file
    // ownership perms govern that path.)
    let config_path = daemon.config_path();
    let original_perms = std::fs::metadata(&config_path)
        .expect("metadata")
        .permissions();
    let mut readonly = original_perms.clone();
    readonly.set_mode(0o400); // r--------
    std::fs::set_permissions(&config_path, readonly).expect("chmod file readonly");

    // Attempt to update the body. Persistence will fail; the call should
    // surface IoError and the in-memory state should roll back.
    let mut mutated = pre_mutation.clone();
    mutated.on_shutdown = Some(SessionAction::ScreenOff);
    let err = client
        .update_system_profile(created, &mutated)
        .await
        .expect_err("persist must fail under readonly config dir");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("ioerror") || chain.contains("io error") || chain.contains("failed to save"),
        "expected IoError on persist failure; got: {chain}",
    );

    // Restore file permissions so subsequent reads / tempdir cleanup
    // work normally.
    std::fs::set_permissions(&config_path, original_perms).expect("restore perms");

    // Refetch confirms in-memory rollback.
    let after = client.get_system_profile(created).await.expect("refetch");
    assert_eq!(
        after.on_shutdown, pre_mutation.on_shutdown,
        "in-memory on_shutdown must roll back on persist failure",
    );
    assert_eq!(
        after.on_startup, pre_mutation.on_startup,
        "unrelated fields must remain untouched",
    );

    // Drain any pending signals with a short timeout. None should arrive.
    let drain = tokio::time::timeout(Duration::from_millis(100), changed_stream.next()).await;
    assert!(
        drain.is_err(),
        "no SystemProfileChanged signal should fire on failed update; got: {drain:?}",
    );
}
