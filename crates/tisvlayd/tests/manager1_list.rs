//! `Manager1.ListSystemProfiles` returns the
//! reserved default profile plus every body created via the CRUD
//! surface, keyed by UUID. Each entry's body matches what
//! `GetSystemProfile` would return for the same UUID — the catalog
//! list and the per-UUID fetch are projections of the same in-memory
//! state.

#![cfg(feature = "mock")]

use tisvlay_core::{DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SystemConfig};
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
async fn list_includes_default_profile_only_when_empty() {
    let (_daemon, client) = daemon_and_client().await;

    let bodies = client.list_system_profile_bodies().await.expect("list");
    assert_eq!(
        bodies.len(),
        1,
        "fresh daemon should expose only default; got {bodies:?}"
    );
    let default = bodies
        .get(&DEFAULT_SYSTEM_PROFILE_UUID)
        .expect("default UUID must be present");
    assert_eq!(default.name, DEFAULT_PROFILE_NAME);
}

#[tokio::test]
async fn list_includes_created_profiles_alongside_default() {
    let (_daemon, client) = daemon_and_client().await;

    let guest_uuid = client
        .create_system_profile("guest")
        .await
        .expect("create guest");
    let gaming_uuid = client
        .create_system_profile("gaming")
        .await
        .expect("create gaming");

    let bodies = client.list_system_profile_bodies().await.expect("list");
    assert_eq!(bodies.len(), 3, "default + 2 created; got {bodies:?}");

    let guest_body = bodies.get(&guest_uuid).expect("guest body");
    assert_eq!(guest_body.name, "guest");
    let gaming_body = bodies.get(&gaming_uuid).expect("gaming body");
    assert_eq!(gaming_body.name, "gaming");
    // Default still resolves to the reserved label.
    assert_eq!(
        bodies
            .get(&DEFAULT_SYSTEM_PROFILE_UUID)
            .expect("default")
            .name,
        DEFAULT_PROFILE_NAME,
    );

    // Newly-created profiles are minimal (only `name` is set; all
    // session-action fields are None so they inherit from default).
    assert!(guest_body.on_startup.is_none());
    assert!(guest_body.on_shutdown.is_none());
    assert!(guest_body.automate_when.is_none());
}

#[tokio::test]
async fn list_entries_match_per_uuid_fetch() {
    let (_daemon, client) = daemon_and_client().await;

    let custom_uuid = client
        .create_system_profile("custom")
        .await
        .expect("create");

    let bodies = client.list_system_profile_bodies().await.expect("list");
    let list_body = bodies.get(&custom_uuid).expect("custom body in list");

    let fetched = client
        .get_system_profile(custom_uuid)
        .await
        .expect("get_system_profile");
    assert_eq!(list_body.name, fetched.name);
    assert_eq!(list_body.on_startup, fetched.on_startup);
    assert_eq!(list_body.on_shutdown, fetched.on_shutdown);
    assert_eq!(list_body.automate_when, fetched.automate_when);
}
