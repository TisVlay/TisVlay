//! End-to-end pairing round-trip through the D-Bus interface.
//!
//! Exercises the same code path the CLI `tisvlay pair` command uses:
//!
//!   DaemonClient::pair_tv()
//!     → Manager1.AddTv (D-Bus, returns tv_id immediately)
//!     → TvManager::pair_tv() (spawns run_pair_task)
//!     → MockConnector::start_pairing() (pre-seeded accept)
//!     → run_pair_task persists, installs, registers, emits
//!       PairingProgress("complete")
//!
//! Tests subscribe to `Manager1.PairingProgress` *before* calling
//! `pair_tv` and drain to the terminal phase via
//! `DaemonClient::drain_pair_to_terminal` — that single round-trip
//! guarantees the spawned task has finished registering the per-TV
//! D-Bus interfaces by the time the client moves on.

#![cfg(feature = "mock")]

use tisvlay_core::{DaemonClient, PairTerminal};
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

    // Subscribe to PairingProgress before kicking off the pair so we
    // catch the terminal phase from the spawned task.
    let stream = client.pairing_progress_stream().await.unwrap();

    // Pair through the D-Bus interface — same call the CLI makes.
    // Returns the reserved tv_id immediately; the handshake itself
    // runs in the daemon's spawned task.
    let config_json = r#"{"address": "mock://living_room"}"#;
    let tv_id = client
        .pair_tv("Living Room", "mock", config_json, true)
        .await
        .expect("AddTv RPC should succeed (sync validation)");

    // AddTv returns a freshly generated UUID v4 — opaque, but it must
    // round-trip to the Tv1.Uuid property from list_tvs().
    assert!(
        uuid::Uuid::parse_str(&tv_id).is_ok(),
        "AddTv should return a valid UUID, got {tv_id:?}"
    );

    // Drain to the terminal phase. `complete` only fires after the
    // spawned task has finished installing + registering, so by the
    // time this returns the per-TV interfaces are live.
    let terminal = DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .expect("drain should reach terminal");
    assert!(
        matches!(terminal, PairTerminal::Complete { .. }),
        "expected Complete, got {terminal:?}"
    );

    // The paired TV should now appear in the list.
    let tvs = client.list_tvs().await.unwrap();
    assert_eq!(tvs.len(), 1);
    assert_eq!(tvs[0].uuid, tv_id);
    assert_eq!(tvs[0].name, "Living Room");
    assert_eq!(tvs[0].vendor, "mock");
    assert!(tvs[0].managed, "should be managed (we passed managed=true)");
    assert!(tvs[0].connected, "should auto-connect after pairing");
}

#[tokio::test]
async fn pair_with_blank_name_defaults_to_device_osd_name() {
    let (daemon, client) = empty_daemon().await;

    // The device reports a model (CEC reads it as the TV's OSD name). Seed it
    // before pairing so the post-pair `hardware_id` read finds it.
    daemon.handle().simulate_user_accept("mock://autofill");
    daemon.handle().set_hardware_id(
        "mock://autofill",
        tisvlay_core::HardwareId {
            manufacturer: Some("LG".to_string()),
            model: Some("OLED42C3PU".to_string()),
            serial: None,
        },
    );

    let stream = client.pairing_progress_stream().await.unwrap();
    // Pair with a *blank* friendly name — the daemon should fall back to the
    // device's model rather than leaving it empty.
    let config_json = r#"{"address": "mock://autofill"}"#;
    let tv_id = client
        .pair_tv("", "mock", config_json, true)
        .await
        .expect("AddTv RPC should succeed");
    DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .expect("drain should reach terminal");

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).expect("paired tv");
    assert_eq!(
        tv.name, "OLED42C3PU",
        "a blank name defaults to the device's model",
    );
}

#[tokio::test]
async fn pair_then_control_screen() {
    let (daemon, client) = empty_daemon().await;

    daemon.handle().simulate_user_accept("mock://office");

    let stream = client.pairing_progress_stream().await.unwrap();
    let config_json = r#"{"address": "mock://office"}"#;
    let tv_id = client
        .pair_tv("Office TV", "mock", config_json, true)
        .await
        .unwrap();
    assert!(
        uuid::Uuid::parse_str(&tv_id).is_ok(),
        "AddTv should return a valid UUID, got {tv_id:?}"
    );
    let terminal = DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .unwrap();
    assert!(matches!(terminal, PairTerminal::Complete { .. }));

    // Screen should default to on.
    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).unwrap();
    assert!(tv.is_screen_on);

    // Turn screen off via D-Bus — proves the paired TV's interfaces
    // are registered and functional.
    client.screen_off(&tv_id).await.unwrap();

    // Brief settle for state forwarder.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).unwrap();
    assert!(!tv.is_screen_on, "screen should be off after screen_off");
}

#[tokio::test]
async fn pair_with_rejected_pairing_returns_error() {
    let (daemon, client) = empty_daemon().await;

    daemon.handle().simulate_user_reject("mock://denied");

    let stream = client.pairing_progress_stream().await.unwrap();
    let config_json = r#"{"address": "mock://denied"}"#;

    // AddTv itself succeeds (sync validation passes). The failure
    // surfaces via the PairingProgress::failed terminal phase.
    let tv_id = client
        .pair_tv("Denied TV", "mock", config_json, true)
        .await
        .expect("AddTv RPC should succeed (sync validation); handshake fails async");

    let terminal = DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .expect("drain should reach terminal");
    let message = match terminal {
        PairTerminal::Failed { message, .. } => message,
        PairTerminal::Complete { .. } => {
            panic!("expected Failed terminal, got Complete")
        }
    };
    assert!(
        message.to_lowercase().contains("reject") || message.to_lowercase().contains("denied"),
        "failure message should mention rejection; got {message:?}"
    );

    // TV should NOT appear in the list — run_pair_task only inserts
    // on the handshake-Ok path.
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

    let stream = client.pairing_progress_stream().await.unwrap();
    let config_json = r#"{"address": "mock://living_room"}"#;
    let tv_id = client
        .pair_tv("Living Room", "mock", config_json, true)
        .await
        .expect("pairing RPC should succeed");
    let terminal = DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .unwrap();
    assert!(matches!(terminal, PairTerminal::Complete { .. }));

    // Brief settle for state forwarder to process any queued
    // subscription updates that arrived during pair_task.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).unwrap();

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

    let stream = client.pairing_progress_stream().await.unwrap();
    let config_json = r#"{"address": "mock://bedroom"}"#;
    let tv_id = client
        .pair_tv("Bedroom", "mock", config_json, false)
        .await
        .unwrap();
    assert!(
        uuid::Uuid::parse_str(&tv_id).is_ok(),
        "AddTv should return a valid UUID, got {tv_id:?}"
    );
    let terminal = DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .unwrap();
    assert!(matches!(terminal, PairTerminal::Complete { .. }));

    let tvs = client.list_tvs().await.unwrap();
    assert_eq!(tvs.len(), 1);
    assert!(
        !tvs[0].managed,
        "should be unmanaged (we passed managed=false)"
    );
}
