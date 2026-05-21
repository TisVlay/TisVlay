//! `Manager1.CreateSystemProfile(name)` mints a fresh UUID, inserts a
//! minimal body (only `name` set), persists, and emits
//! `SystemProfileAdded(uuid)`. Empty name / reserved `"default"` /
//! duplicate-of-existing-name all reject with
//! `org.tisvlay.Error.ProfileNameInvalid`.

#![cfg(feature = "mock")]

use tisvlay_core::{DaemonClient, SystemConfig};
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
async fn create_returns_uuid_and_minimal_body() {
    let (_daemon, client) = daemon_and_client().await;
    let new_uuid = client
        .create_system_profile("guest")
        .await
        .expect("create guest");

    // Body is minimal — every Option<T> defaults to None.
    let body = client.get_system_profile(new_uuid).await.expect("get body");
    assert_eq!(body.name, "guest");
    assert!(body.on_startup.is_none());
    assert!(body.on_shutdown.is_none());
    assert!(body.on_reboot.is_none());
    assert!(body.on_sleep.is_none());
    assert!(body.on_wake.is_none());
    assert!(body.automate_when.is_none());

    // Listing includes the new profile.
    let bodies = client.list_system_profile_bodies().await.unwrap();
    assert!(
        bodies.contains_key(&new_uuid),
        "newly-created profile should appear in ListSystemProfiles; got {bodies:?}",
    );
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let (_daemon, client) = daemon_and_client().await;
    let err = client
        .create_system_profile("")
        .await
        .expect_err("empty name should reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("non-empty"),
        "expected ProfileNameInvalid for empty name; got: {chain}",
    );
}

#[tokio::test]
async fn create_rejects_whitespace_only_name() {
    let (_daemon, client) = daemon_and_client().await;
    let err = client
        .create_system_profile("   ")
        .await
        .expect_err("whitespace-only name should reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("non-empty"),
        "expected ProfileNameInvalid for whitespace-only name; got: {chain}",
    );
}

#[tokio::test]
async fn create_rejects_reserved_default_literal() {
    let (_daemon, client) = daemon_and_client().await;
    let err = client
        .create_system_profile("default")
        .await
        .expect_err("reserved literal should reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("reserved"),
        "expected ProfileNameInvalid for 'default'; got: {chain}",
    );
}

#[tokio::test]
async fn create_rejects_duplicate_of_existing_name() {
    let (_daemon, client) = daemon_and_client().await;
    client
        .create_system_profile("studio")
        .await
        .expect("first create");

    let err = client
        .create_system_profile("studio")
        .await
        .expect_err("duplicate name should reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("already in use"),
        "expected ProfileNameInvalid for duplicate name; got: {chain}",
    );
}
