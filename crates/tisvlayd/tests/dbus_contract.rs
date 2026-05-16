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

// ---------- Inherently-reliable screen_on / screen_off ----------

/// `screen_on` short-circuits straight to `power_on` when cached
/// `last_known_power_state == "standby"`. Skips the verify-retry
/// loop entirely (TV is past Screen Off; `screen_on` over the
/// existing WS would no-op or stall).
///
/// `inject_power_state(Standby)` → cached `"standby"`. Dispatch
/// `Tv1.Power.PowerOn`'s neighbour, but for screen_on go via the
/// daemon's screen_on path: call directly via the manager since
/// `Tv1.Screen.SetScreen(true)` is the user-facing path. Assert
/// the TV ends `"active"` (short-circuit → power_on → fresh
/// MockConnection with `initial_power_state = "active"`).
#[tokio::test]
async fn screen_on_short_circuits_from_standby() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_on_sc")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_on_sc").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject Standby → cached PowerState `"standby"`. The mock
    // state's `is_on=false`; `state_forwarder` derives `"standby"`.
    daemon
        .handle()
        .inject_power_state("mock://scr_on_sc", PowerState::Standby);
    settle().await;

    // Drive screen_on through the manager (matches what the
    // user-facing D-Bus `Tv1.Screen.SetScreen(true)` would do).
    daemon.manager().screen_on(&uuid).await.expect("screen_on");

    // Short-circuit budget: power_on probe + WoL + reconnect.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "short-circuit should have dispatched power_on; new connection comes up active",
    );
    assert!(
        tv.is_screen_on,
        "panel should be on after escalated power_on"
    );
}

/// `screen_on` runs verify-retry-escalate when cached state is
/// not `"standby"` (so the short-circuit doesn't fire) and the
/// mock's `screen_on` is no-op'd (so verify can't pass on
/// attempt #1). Mirrors LG firmware refusing to wake from
/// `ActiveStandby`. Asserts the TV ends `"active"` via the
/// escalation path after retries exhaust.
#[tokio::test]
async fn screen_on_escalates_via_verify_retry_loop() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_on_esc")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_on_esc").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject ActiveStandby → derives daemon-side cached
    // `"screen_off"` (Bug A fix: `is_on=true, is_screen_on=false`),
    // so the standby short-circuit deliberately does NOT fire.
    daemon
        .handle()
        .inject_power_state("mock://scr_on_esc", PowerState::ActiveStandby);
    settle().await;

    // Without this knob, mock's default `screen_on` flips
    // `is_screen_on=true`, cached state goes to `"active"`, and
    // verify passes on attempt #1 — defeating the escalation
    // path this test means to exercise.
    daemon
        .handle()
        .set_screen_on_noop("mock://scr_on_esc", true);

    daemon.manager().screen_on(&uuid).await.expect("screen_on");

    // Verify-retry path: 2 × 300 ms verify + power_on probe +
    // new-connection state push.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "verify-retry exhausted should escalate to power_on; new connection comes up active",
    );
    assert!(
        tv.is_screen_on,
        "panel should be on after escalated power_on"
    );
}

/// `screen_off` pre-flight escalates via `power_on` when cached
/// state reads `"standby"`. The user wants TV in screen-off
/// substate; for that to be meaningful, TV must be powered on
/// first. Serves the `on_startup = "screen_off"` usecase. After
/// power_on, the screen_off command lands on the live connection
/// and verify reads cached `"screen_off"`.
#[tokio::test]
async fn screen_off_escalates_via_power_on_when_standby() {
    use tisvlay_core::PowerState;

    let daemon = TestDaemon::with_one_mock_tv("scr_off_esc")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_esc").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Inject Standby → cached `"standby"`. Pre-flight escalates
    // via power_on, then sends screen_off on the fresh connection.
    daemon
        .handle()
        .inject_power_state("mock://scr_off_esc", PowerState::Standby);
    settle().await;

    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    // Budget: power_on probe + WoL + reconnect + screen_off +
    // verify-delay.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "screen_off",
        "screen_off after pre-flight power_on should land cached PowerState in 'screen_off'",
    );
    assert!(!tv.is_screen_on, "panel should be off after screen_off");
}

/// `screen_off` runs the verify-retry loop when cached state is
/// already `"active"` and the mock's `screen_off` is no-op'd —
/// models firmware that ACKs the off command but doesn't blank.
/// No escalation past retries (TV is reachable; we log and give
/// up). Asserts the call returns `Ok` and panel stays on.
#[tokio::test]
async fn screen_off_verify_retry_gives_up_when_panel_refuses() {
    let daemon = TestDaemon::with_one_mock_tv("scr_off_rf")
        .await
        .expect("daemon");
    let client = client().await;
    let uuid = test_tv_uuid("scr_off_rf").to_string();

    daemon.manager().reconnect(&uuid).await.expect("reconnect");
    settle().await;

    // Mock starts in `"active"`. Without the noop knob, default
    // `screen_off` flips `is_screen_on=false` → cached
    // `"screen_off"` → verify passes on attempt #1, not what we
    // want to test here.
    daemon
        .handle()
        .set_screen_off_noop("mock://scr_off_rf", true);

    // `Ok` — the helper gives up gracefully on retry exhaustion
    // for screen_off (no escalation path).
    daemon
        .manager()
        .screen_off(&uuid)
        .await
        .expect("screen_off");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "noop'd screen_off should leave cached state unchanged at 'active'",
    );
    assert!(tv.is_screen_on, "noop'd screen_off should leave panel on");
}
