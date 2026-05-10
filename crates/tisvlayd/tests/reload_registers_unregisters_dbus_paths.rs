//! Regression: editing `config.toml` to add or remove a `[[tv]]` block
//! and calling `Manager1.ReloadConfig` must register / unregister the
//! corresponding per-TV D-Bus object paths so `ObjectManager`'s view
//! stays in sync with the on-disk config.
//!
//! Pre-fix behavior: `tv_manager::reload_config` updated the in-memory
//! `IndexMap` (and closed connections on removed TVs) but never touched
//! D-Bus paths. Vanished TVs persisted as ghost paths in
//! `ObjectManager.GetManagedObjects` (visible to GUI + Tray clients,
//! with method calls failing behind the scenes); newly-added TVs from
//! file edits were tracked in the manager but invisible on the bus.
//!
//! Both directions are checked here. The user-reported case is the
//! removal half; the addition half was found during diagnosis and is
//! the symmetric counterpart.

#![cfg(feature = "mock")]

use std::time::Duration;

use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::{TestDaemon, mock_config_with_tvs};
use tisvlay_test::PrivateBus;

/// Build a fresh `DaemonClient` over the same private bus the test
/// daemon is serving on, so calls hit the in-process daemon and not
/// the user's real session bus.
async fn client() -> DaemonClient {
    let bus_conn = PrivateBus::shared().connect().await.expect("client conn");
    DaemonClient::from_connection(bus_conn)
        .await
        .expect("daemon client")
}

/// `state_forwarder` consumes `ConfigReloaded` asynchronously; the
/// dbus-handler-driven path-register/unregister calls happen
/// synchronously inside `reload_config`, but the property-refresh
/// side effects are queued. Mirrors the settle window in
/// `reload_resolves_system_profile.rs`.
const SETTLE: Duration = Duration::from_millis(50);

#[tokio::test]
async fn reload_unregisters_dbus_path_for_removed_tv() {
    let initial = mock_config_with_tvs(&["living_room", "bedroom"]);
    let daemon = TestDaemon::with_config(initial).await.expect("test daemon");

    let client = client().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(
        tvs.len(),
        2,
        "sanity: both configured TVs visible via ObjectManager after startup"
    );

    // Edit on-disk config: drop "bedroom", keep "living_room".
    let updated = mock_config_with_tvs(&["living_room"]);
    updated
        .save_to(&daemon.config_path())
        .expect("write updated config");

    // Drive reload through the D-Bus client, not `daemon.manager()`
    // directly: the path-register/unregister logic lives in the
    // `Manager1.ReloadConfig` handler, which is what production
    // callers (GUI, Tray, busctl) invoke.
    client
        .reload_config()
        .await
        .expect("reload_config should succeed");

    tokio::time::sleep(SETTLE).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(
        tvs.len(),
        1,
        "after reload, removed TV's D-Bus path should be unregistered \
         so ObjectManager stops reporting it — the GUI and Tray populate \
         from this list, and a ghost path here is what makes a deleted \
         TV linger in the UI",
    );
    assert_eq!(
        tvs[0].name, "living_room",
        "remaining TV should be living_room"
    );
}

#[tokio::test]
async fn reload_registers_dbus_path_for_added_tv() {
    let initial = mock_config_with_tvs(&["living_room"]);
    let daemon = TestDaemon::with_config(initial).await.expect("test daemon");

    let client = client().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(
        tvs.len(),
        1,
        "sanity: one TV visible via ObjectManager after startup"
    );

    // Edit on-disk config: append "bedroom".
    let updated = mock_config_with_tvs(&["living_room", "bedroom"]);
    updated
        .save_to(&daemon.config_path())
        .expect("write updated config");

    // Drive reload through the D-Bus client, not `daemon.manager()`
    // directly: the path-register/unregister logic lives in the
    // `Manager1.ReloadConfig` handler, which is what production
    // callers (GUI, Tray, busctl) invoke.
    client
        .reload_config()
        .await
        .expect("reload_config should succeed");

    tokio::time::sleep(SETTLE).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(
        tvs.len(),
        2,
        "after reload, the newly-added TV's D-Bus path should be \
         registered so ObjectManager surfaces it — without this, a TV \
         added via direct config-file edit is invisible to clients \
         until the daemon restarts",
    );
    assert!(
        tvs.iter().any(|t| t.name == "bedroom"),
        "bedroom should be among the visible TVs"
    );
}
