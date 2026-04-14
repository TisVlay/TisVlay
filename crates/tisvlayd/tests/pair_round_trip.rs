//! End-to-end pairing round-trip through the D-Bus interface.
//!
//! Exercises the same code path the CLI `tisvlay pair` command uses:
//!
//!   DaemonClient::pair_tv()
//!     → Manager1.AddTv (D-Bus)
//!     → TvManager::pair_tv()
//!     → MockConnector::start_pairing() (pre-seeded accept)
//!     → save config + register D-Bus interfaces
//!
//! The paired TV should appear in the TV list with correct properties,
//! and the client should be able to control it immediately.

#![cfg(feature = "mock")]

use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::TestDaemon;

/// Build a TestDaemon with no pre-configured TVs — a clean slate for
/// pairing. Uses the same `create_test_registry` so the mock connector
/// is wired in.
async fn empty_daemon() -> (TestDaemon, DaemonClient) {
    let config = tisvlay_core::SystemConfig::default();
    let daemon = TestDaemon::with_config(config).await.unwrap();
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .unwrap();
    (daemon, client)
}

#[tokio::test]
async fn pair_mock_tv_appears_in_tv_list() {
    let (daemon, client) = empty_daemon().await;

    // Pre-seed mock with an accept for this address.
    daemon.handle().simulate_user_accept("mock://living_room");

    // Pair through the D-Bus interface — same call the CLI makes.
    let config_json = r#"{"address": "mock://living_room"}"#;
    let tv_id = client
        .pair_tv("Living Room", "mock", config_json, true)
        .await
        .expect("pairing should succeed");

    // ID is derived from name: "Living Room" → "living_room" (underscores,
    // not hyphens). AddTv now returns the canonical ID directly, so it
    // matches the Tv1.Id property from list_tvs().
    assert_eq!(tv_id, "living_room");

    // The paired TV should now appear in the list.
    let tvs = client.list_tvs().await.unwrap();
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].id, "living_room");
    assert_eq!(tvs[0].name, "Living Room");
    assert_eq!(tvs[0].vendor, "mock");
    assert!(tvs[0].managed, "should be managed (we passed managed=true)");
    assert!(tvs[0].connected, "should auto-connect after pairing");
}

#[tokio::test]
async fn pair_then_control_screen() {
    let (daemon, client) = empty_daemon().await;

    daemon.handle().simulate_user_accept("mock://office");

    let config_json = r#"{"address": "mock://office"}"#;
    let tv_id = client
        .pair_tv("Office TV", "mock", config_json, true)
        .await
        .unwrap();
    assert_eq!(tv_id, "office_tv");

    // Screen should default to on.
    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.id == tv_id).unwrap();
    assert!(tv.is_screen_on);

    // Turn screen off via D-Bus — proves the paired TV's interfaces
    // are registered and functional.
    client.screen_off(&tv_id).await.unwrap();

    // Brief settle for state forwarder.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.id == tv_id).unwrap();
    assert!(!tv.is_screen_on, "screen should be off after screen_off");
}

#[tokio::test]
async fn pair_with_rejected_pairing_returns_error() {
    let (daemon, client) = empty_daemon().await;

    daemon.handle().simulate_user_reject("mock://denied");

    let config_json = r#"{"address": "mock://denied"}"#;
    let result = client.pair_tv("Denied TV", "mock", config_json, true).await;

    assert!(result.is_err(), "pairing should fail when user rejects");

    // TV should NOT appear in the list.
    let tvs = client.list_tvs().await.unwrap();
    assert!(tvs.is_empty(), "rejected TV should not be in list");
}

/// Regression test for https://github.com/ruffsl/TisVlay/issues/21
///
/// After pairing, the D-Bus properties should reflect the mock's actual
/// state (volume=30, backlight=80, input=HDMI_1, power=active), not
/// zeroed defaults (volume=0, backlight=0, input="", power="unknown").
///
/// The bug: `register_tv_interfaces()` created D-Bus interface structs
/// with hardcoded defaults, and the initial subscription updates from
/// `subscribe_state()` arrived before the interfaces were registered,
/// so the state-change forwarder silently dropped them.
#[tokio::test]
async fn pair_state_reflects_mock_defaults_not_zeroed() {
    let (daemon, client) = empty_daemon().await;

    daemon.handle().simulate_user_accept("mock://living_room");

    let config_json = r#"{"address": "mock://living_room"}"#;
    let tv_id = client
        .pair_tv("Living Room", "mock", config_json, true)
        .await
        .expect("pairing should succeed");

    // Brief settle for state forwarder to process any queued updates.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.id == tv_id).unwrap();

    // Mock defaults: volume=30, backlight=80, input=HDMI_1, power=active.
    // If the bug is present, these will be 0/"unknown"/"" instead.
    assert_eq!(
        tv.volume, 30,
        "volume should be mock default (30), not zeroed"
    );
    assert_eq!(
        tv.backlight, 80,
        "backlight should be mock default (80), not zeroed"
    );
    assert_eq!(
        tv.current_input, "HDMI_1",
        "input should be mock default (HDMI_1), not empty"
    );
    assert_eq!(
        tv.power_state, "active",
        "power should be mock default (active), not unknown"
    );
    assert!(
        tv.is_screen_on,
        "screen should be on (mock default), not off"
    );
}

#[tokio::test]
async fn pair_unmanaged_tv() {
    let (daemon, client) = empty_daemon().await;

    daemon.handle().simulate_user_accept("mock://bedroom");

    let config_json = r#"{"address": "mock://bedroom"}"#;
    let tv_id = client
        .pair_tv("Bedroom", "mock", config_json, false)
        .await
        .unwrap();
    assert_eq!(tv_id, "bedroom");

    let tvs = client.list_tvs().await.unwrap();
    assert_eq!(tvs.len(), 1);
    assert!(
        !tvs[0].managed,
        "should be unmanaged (we passed managed=false)"
    );
}
