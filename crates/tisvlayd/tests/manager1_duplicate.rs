//! `Manager1.DuplicateSystemProfile(source_uuid, new_name)` clones an
//! existing profile under a new UUID + name. The source body stays
//! untouched.

#![cfg(feature = "mock")]

use tisvlay_core::{AutomateWhen, DaemonClient, SessionAction, SystemConfig};
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
async fn duplicate_clones_source_body_under_new_name() {
    let (_daemon, client) = daemon_and_client().await;

    // Create + populate a source profile.
    let source = client
        .create_system_profile("base")
        .await
        .expect("create source");
    let mut body = client.get_system_profile(source).await.unwrap();
    body.on_startup = Some(SessionAction::PowerOn);
    body.on_shutdown = Some(SessionAction::PowerOff);
    body.automate_when = Some(AutomateWhen::OnPrimary);
    client.update_system_profile(source, &body).await.unwrap();

    // Duplicate under a new name.
    let copy = client
        .duplicate_system_profile(source, "copy")
        .await
        .expect("duplicate");
    assert_ne!(copy, source, "duplicate must mint a fresh UUID");

    // Copy body carries every non-name field from the source.
    let copy_body = client.get_system_profile(copy).await.unwrap();
    assert_eq!(copy_body.name, "copy", "copy carries new name");
    assert_eq!(copy_body.on_startup, Some(SessionAction::PowerOn));
    assert_eq!(copy_body.on_shutdown, Some(SessionAction::PowerOff));
    assert_eq!(copy_body.automate_when, Some(AutomateWhen::OnPrimary));

    // Source body is untouched.
    let source_after = client.get_system_profile(source).await.unwrap();
    assert_eq!(source_after.name, "base");
    assert_eq!(source_after.on_startup, Some(SessionAction::PowerOn));
    assert_eq!(source_after.on_shutdown, Some(SessionAction::PowerOff));
    assert_eq!(source_after.automate_when, Some(AutomateWhen::OnPrimary));
}

#[tokio::test]
async fn duplicate_rejects_duplicate_name() {
    let (_daemon, client) = daemon_and_client().await;
    let source = client.create_system_profile("source").await.unwrap();
    let _other = client.create_system_profile("taken").await.unwrap();

    let err = client
        .duplicate_system_profile(source, "taken")
        .await
        .expect_err("name collision should reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenameinvalid") || chain.contains("already in use"),
        "expected ProfileNameInvalid on duplicate-name collision; got: {chain}",
    );
}

#[tokio::test]
async fn duplicate_rejects_unknown_source_uuid() {
    let (_daemon, client) = daemon_and_client().await;
    let absent = uuid::Uuid::new_v4();
    let err = client
        .duplicate_system_profile(absent, "any")
        .await
        .expect_err("unknown source UUID should reject");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilenotfound") || chain.contains("not found"),
        "expected ProfileNotFound for unknown source; got: {chain}",
    );
}
