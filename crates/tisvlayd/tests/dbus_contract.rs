//! D-Bus contract tests.
//!
//! Verify that the daemon's D-Bus surface correctly reflects backend
//! state and that client round-trips (read → command → read) through
//! the full zbus pipeline produce the expected property values.
//!
//! These tests catch regressions in:
//! - `InterfaceRef::get_mut()` property cache updates
//! - `PropertiesChanged` signal emission via the `_changed()` helpers
//! - The state-change forwarder's field mapping from `TvState` to
//!   per-interface cached properties

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use tisvlay_test::PrivateBus;

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn client() -> DaemonClient {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    DaemonClient::from_connection(conn)
        .await
        .expect("daemon client")
}

// ---------- Tv1.Dispatch ----------

/// `Tv1.Dispatch` with reliable QoS on `screen_on` engages the
/// daemon-side verify-retry-escalate helper. When the cached
/// PowerState fails to reach `"active"` (here: we inject `Standby`
/// via the mock handle to simulate the TV slipping past Screen Off),
/// the helper falls through to `power_on` — the new connection comes
/// up `"active"` per the mock's default `initial_power_state`.
///
/// End-to-end contract for the QoS-dispatch refactor: session events
/// flow through this same code path with the resolved per-event QoS
/// (migrated in a follow-up commit), and direct callers use the
/// `DaemonClient::screen_on` / `power_on` Rust wrappers that pass
/// best-effort QoS to the same method.
#[tokio::test]
async fn dispatch_reliable_screen_on_escalates_via_active_standby() {
    use tisvlay_core::{PowerState, QosConfig, Reliability, SessionAction};

    let daemon = TestDaemon::with_one_mock_tv("dispatch_escalate")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("dispatch_escalate").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Simulate the TV transitioning out of Screen Off into Active
    // Standby — same wire shape as the field-test repro on the LG G4.
    daemon
        .handle()
        .inject_power_state("mock://dispatch_escalate", PowerState::ActiveStandby);
    settle().await;

    // Model LG firmware's "screen_on returns Ok but doesn't wake the
    // panel" behavior in ActiveStandby. Without this, mock's default
    // `is_screen_on = true` flip lets verify pass on attempt #1 and
    // this test would exercise verify-success rather than the
    // escalation path it asserts.
    daemon
        .handle()
        .set_screen_on_noop("mock://dispatch_escalate", true);

    let qos = QosConfig {
        reliability: Some(Reliability::Reliable),
    };
    client
        .dispatch_action(&uuid, SessionAction::ScreenOn, qos)
        .await
        .expect("dispatch_action");

    // Reliable path: 2 × 300 ms verify + power_on probe + new-connection
    // state push. Match the budget the session-events integration test
    // uses (1.5 s).
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs
        .iter()
        .find(|t| t.uuid == uuid)
        .expect("dispatch_escalate TV present");
    assert_eq!(
        tv.power_state, "active",
        "reliable dispatch should have escalated to power_on; new connection comes up active",
    );
    assert!(
        tv.is_screen_on,
        "screen should be on after the escalated power_on completes",
    );
}

/// Reliable `screen_on` short-circuits straight to `power_on` when
/// `last_known_power_state` is already `"standby"` (TV's `is_on ==
/// false`, i.e., past Screen Off — the daemon's derived shorthand
/// for `Standby` / `Suspend` / `PowerOff`). Skips the verify-retry
/// loop entirely, avoiding the 10 s `REQUEST_MS` exposure when the
/// LG TV's WebSocket has half-died with the TV.
///
/// Mock injects `PowerState::Standby` to put cached state at
/// `"standby"`. Dispatch with reliable QoS. Asserts the TV ends up
/// `"active"` via the escalation path. Pairs with the
/// `dispatch_reliable_screen_on_escalates_via_active_standby` test
/// above, which exercises the verify-retry-escalate path for the
/// `screen_off` cached-state input that the short-circuit explicitly
/// leaves untouched.
#[tokio::test]
async fn dispatch_reliable_screen_on_short_circuits_from_standby() {
    use tisvlay_core::{PowerState, QosConfig, Reliability, SessionAction};

    let daemon = TestDaemon::with_one_mock_tv("dispatch_shortcircuit")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("dispatch_shortcircuit").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject Standby — TV `is_on == false`, derives daemon-side cached
    // PowerState `"standby"`. This is the state where `screen_on` over
    // an existing WebSocket would either no-op or block 10 s waiting
    // for a reply that never comes; the pre-flight check should
    // bypass it and dispatch power_on directly.
    daemon
        .handle()
        .inject_power_state("mock://dispatch_shortcircuit", PowerState::Standby);
    settle().await;

    let qos = QosConfig {
        reliability: Some(Reliability::Reliable),
    };
    client
        .dispatch_action(&uuid, SessionAction::ScreenOn, qos)
        .await
        .expect("dispatch_action");

    // Short-circuit budget: power_on probe + WoL + new-connection
    // state push. No verify-retry sleeps, so this is tighter than the
    // verify-retry test's 1.5 s but matched here for parity.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs
        .iter()
        .find(|t| t.uuid == uuid)
        .expect("dispatch_shortcircuit TV present");
    assert_eq!(
        tv.power_state, "active",
        "short-circuit should have dispatched power_on; new connection comes up active",
    );
    assert!(
        tv.is_screen_on,
        "screen should be on after the escalated power_on completes",
    );
}

// ---------- Effective system profile JSON ----------

#[tokio::test]
async fn get_effective_system_profile_returns_resolved_json() {
    let _daemon = TestDaemon::with_one_mock_tv("eff_sys_tv")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("eff_sys_tv").to_string();

    let json: String = client
        .get_effective_system_profile(&uuid)
        .await
        .expect("get_effective_system_profile");

    // Each on_* is a nested `{ action, qos: { reliability } }` table
    // mirroring the TOML shape. The asymmetric default is type-driven:
    // wake-side actions resolve to reliable, sleep-side to best-effort.
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(value["on_startup"]["action"], "power_on");
    assert_eq!(value["on_startup"]["qos"]["reliability"], "reliable");
    assert_eq!(value["on_shutdown"]["action"], "power_off");
    assert_eq!(value["on_shutdown"]["qos"]["reliability"], "best_effort");
    assert_eq!(value["on_sleep"]["action"], "power_off");
    assert_eq!(value["on_sleep"]["qos"]["reliability"], "best_effort");
    assert_eq!(value["on_wake"]["action"], "power_on");
    assert_eq!(value["on_wake"]["qos"]["reliability"], "reliable");
    assert_eq!(value["automate_when"], "always");
}

// ---------- Initial state after connect ----------

#[tokio::test]
async fn connected_tv_reports_initial_mock_state() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_init")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_init").to_string())
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = &tvs[0];

    assert!(tv.connected);
    assert_eq!(tv.power_state, "active");
    assert!(tv.is_screen_on);
    assert_eq!(tv.volume, 30);
    assert!(!tv.muted);
    assert_eq!(tv.current_input, "HDMI_1");
    assert_eq!(tv.backlight, 80);
    assert!(!tv.inputs.is_empty());
}

// ---------- Command round-trips ----------

#[tokio::test]
async fn set_volume_round_trips_through_dbus() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_vol")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_vol").to_string())
        .await
        .expect("reconnect");
    settle().await;

    let uuid = test_tv_uuid("dbus_vol").to_string();
    client.set_volume(&uuid, 75).await.expect("set_volume");
    settle().await;

    let vol = client.get_volume(&uuid).await.expect("get_volume");
    assert_eq!(vol, 75);
}

#[tokio::test]
async fn set_mute_round_trips_through_dbus() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_mute")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_mute").to_string())
        .await
        .expect("reconnect");
    settle().await;

    client
        .set_mute(&test_tv_uuid("dbus_mute").to_string(), true)
        .await
        .expect("set_mute");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert!(tvs[0].muted);
}

#[tokio::test]
async fn screen_off_updates_power_state_and_screen_flag() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_screen")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_screen").to_string())
        .await
        .expect("reconnect");
    settle().await;

    client
        .screen_off(&test_tv_uuid("dbus_screen").to_string())
        .await
        .expect("screen_off");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert!(!tvs[0].is_screen_on);
    assert_eq!(tvs[0].power_state, "screen_off");
}

#[tokio::test]
async fn set_backlight_round_trips_through_dbus() {
    let daemon = TestDaemon::with_one_mock_tv("dbus_bl")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&test_tv_uuid("dbus_bl").to_string())
        .await
        .expect("reconnect");
    settle().await;

    let uuid = test_tv_uuid("dbus_bl").to_string();
    client
        .set_backlight(&uuid, 42)
        .await
        .expect("set_backlight");
    settle().await;

    let bl = client.get_backlight(&uuid).await.expect("get_backlight");
    assert_eq!(bl, 42);
}

// ---------- Supported vendors (Manager1 surface) ----------

#[tokio::test]
async fn get_supported_vendors_includes_mock() {
    let _daemon = TestDaemon::with_one_mock_tv("dbus_vendors")
        .await
        .expect("daemon");
    let client = client().await;

    let vendors = client
        .get_supported_vendors()
        .await
        .expect("get_supported_vendors");
    assert!(
        vendors.iter().any(|(id, _)| id == "mock"),
        "mock vendor should be in the registry: {vendors:?}",
    );
}
