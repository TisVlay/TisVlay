//! `Manager1.UpdateSystemProfile(uuid, body)` is an atomic full-body
//! replace. The body is the wire's full snapshot — any `Option<T>` field
//! sent as `None` clears the local override (the profile falls back to
//! the default body / hardcoded baseline for that field).

#![cfg(feature = "mock")]

use tisvlay_core::{DaemonClient, SessionAction, SystemConfig};
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
async fn update_round_trips_mutated_field() {
    let (_daemon, client) = daemon_and_client().await;
    let created = client
        .create_system_profile("editable")
        .await
        .expect("create");

    let mut body = client
        .get_system_profile(created)
        .await
        .expect("fetch initial body");
    assert!(body.on_shutdown.is_none());

    body.on_shutdown = Some(SessionAction::ScreenOff);
    client
        .update_system_profile(created, &body)
        .await
        .expect("update should succeed");

    let refetched = client
        .get_system_profile(created)
        .await
        .expect("refetch body");
    assert_eq!(refetched.on_shutdown, Some(SessionAction::ScreenOff));
    assert_eq!(refetched.name, "editable", "name preserved across update");
}

#[tokio::test]
async fn update_clears_optional_field_when_set_to_none() {
    // Atomic replace: sending the body with an `Option<T>` field as
    // `None` is how clients clear a previously-set override.
    let (_daemon, client) = daemon_and_client().await;
    let created = client
        .create_system_profile("clearable")
        .await
        .expect("create");

    // Set on_startup, observe it.
    let mut body = client.get_system_profile(created).await.unwrap();
    body.on_startup = Some(SessionAction::PowerOn);
    client.update_system_profile(created, &body).await.unwrap();
    let mid = client.get_system_profile(created).await.unwrap();
    assert_eq!(mid.on_startup, Some(SessionAction::PowerOn));

    // Now clear it: same body, on_startup = None.
    let mut cleared = mid.clone();
    cleared.on_startup = None;
    client
        .update_system_profile(created, &cleared)
        .await
        .expect("update with None clears the field");

    let after = client.get_system_profile(created).await.unwrap();
    assert!(
        after.on_startup.is_none(),
        "on_startup must be cleared after atomic replace with None; got {:?}",
        after.on_startup,
    );
}

#[tokio::test]
async fn update_preserves_unrelated_fields() {
    // The wire body is the full snapshot, but each test client uses
    // `get → mutate → send` so unrelated fields naturally survive.
    let (_daemon, client) = daemon_and_client().await;
    let created = client.create_system_profile("multi").await.expect("create");

    let mut body = client.get_system_profile(created).await.unwrap();
    body.on_shutdown = Some(SessionAction::PowerOff);
    body.on_sleep = Some(SessionAction::ScreenOff);
    client.update_system_profile(created, &body).await.unwrap();

    let after = client.get_system_profile(created).await.unwrap();
    assert_eq!(after.on_shutdown, Some(SessionAction::PowerOff));
    assert_eq!(after.on_sleep, Some(SessionAction::ScreenOff));
}
