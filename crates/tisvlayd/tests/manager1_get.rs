//! `Manager1.GetSystemProfile(uuid)` round-trips a single body by UUID.
//! Missing UUIDs (well-formed but not in catalog) and malformed UUIDs
//! both surface as `org.tisvlay.Error.ProfileNotFound` — the daemon
//! collapses both into the same error name so clients can route the
//! "unknown profile" case uniformly.

#![cfg(feature = "mock")]

use tisvlay_core::{DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

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
async fn get_returns_default_profile_body() {
    let (_daemon, client) = daemon_and_client().await;
    let body = client
        .get_system_profile(DEFAULT_SYSTEM_PROFILE_UUID)
        .await
        .expect("default profile fetch should succeed");
    assert_eq!(body.name, "default");
    // The default body has explicit values for every Option<T> field
    // (ensure_default + full_default contract).
    assert!(body.on_startup.is_some());
    assert!(body.on_shutdown.is_some());
    assert!(body.automate_when.is_some());
}

#[tokio::test]
async fn get_returns_created_profile_body() {
    let (_daemon, client) = daemon_and_client().await;
    let created = client
        .create_system_profile("gaming")
        .await
        .expect("create gaming");

    let body = client
        .get_system_profile(created)
        .await
        .expect("get_system_profile");
    assert_eq!(body.name, "gaming");
    // Newly-created profile inherits everything from default → all None.
    assert!(body.on_startup.is_none());
    assert!(body.on_shutdown.is_none());
    assert!(body.automate_when.is_none());
}

#[tokio::test]
async fn get_unknown_uuid_returns_profile_not_found() {
    let (_daemon, client) = daemon_and_client().await;
    let absent = Uuid::new_v4();
    let err = client
        .get_system_profile(absent)
        .await
        .expect_err("fetching a non-existent UUID should fail");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("profile not found"),
        "expected ProfileNotFound; got: {chain}",
    );
}

#[tokio::test]
async fn get_malformed_uuid_returns_profile_not_found() {
    let (_daemon, client) = daemon_and_client().await;
    // Drive the underlying proxy directly so we can send a garbage
    // UUID string — the typed `Uuid` wrapper would reject it client-
    // side. Use a manual call against the proxy via the daemon's
    // connection.
    let conn = client.connection().clone();
    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Daemon",
        "/org/tisvlay/Manager",
        "org.tisvlay.Manager1",
    )
    .await
    .expect("proxy");
    let err = proxy
        .call_method("GetSystemProfile", &("not-a-uuid",))
        .await
        .expect_err("malformed UUID should fail");
    let name = err.to_string().to_lowercase();
    assert!(
        name.contains("profilenotfound") || name.contains("profile not found"),
        "expected ProfileNotFound on malformed UUID; got: {err:?}",
    );
}
