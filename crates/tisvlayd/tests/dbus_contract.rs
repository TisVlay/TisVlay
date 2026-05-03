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
