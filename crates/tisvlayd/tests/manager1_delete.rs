//! `Manager1.DeleteSystemProfile(uuid)` drops a body from the catalog
//! and emits `SystemProfileRemoved`. The reserved-default UUID is never
//! deletable.

#![cfg(feature = "mock")]

use tisvlay_core::{DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;

async fn daemon_and_client() -> (TestDaemon, DaemonClient) {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");
    (daemon, client)
}

#[tokio::test]
async fn delete_removes_created_profile_from_catalog() {
    let (_daemon, client) = daemon_and_client().await;
    let created = client.create_system_profile("ephemeral").await.unwrap();

    // Before: present.
    let before = client.list_system_profile_bodies().await.unwrap();
    assert!(before.contains_key(&created));

    client
        .delete_system_profile(created)
        .await
        .expect("delete should succeed");

    // After: gone, only default remains.
    let after = client.list_system_profile_bodies().await.unwrap();
    assert!(!after.contains_key(&created), "deleted UUID must be gone");
    assert!(after.contains_key(&DEFAULT_SYSTEM_PROFILE_UUID));

    // Per-UUID fetch now returns ProfileNotFound.
    let err = client
        .get_system_profile(created)
        .await
        .expect_err("deleted UUID must fetch as ProfileNotFound");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound; got: {chain}",
    );
}

#[tokio::test]
async fn delete_rejects_default_uuid_with_reserved_profile() {
    let (_daemon, client) = daemon_and_client().await;

    let err = client
        .delete_system_profile(DEFAULT_SYSTEM_PROFILE_UUID)
        .await
        .expect_err("default UUID must not be deletable");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("reservedprofile") || chain.contains("reserved"),
        "expected ReservedProfile on default-UUID delete; got: {chain}",
    );

    // Default profile still present.
    let bodies = client.list_system_profile_bodies().await.unwrap();
    assert!(bodies.contains_key(&DEFAULT_SYSTEM_PROFILE_UUID));
}
