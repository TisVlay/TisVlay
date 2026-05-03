//! Integration tests for system-event dispatch through `TvManager`.
//!
//! Each test builds a `TestDaemon` with one mock TV, connects it via
//! `reconnect`, fires a system event, and asserts the resulting TV
//! state via `DaemonClient::list_tvs()` (so the full D-Bus pipeline
//! is exercised: command → mock connection → subscription push →
//! state-change forwarder → D-Bus PropertiesChanged).
//!
//! The default resolved system profile maps:
//!
//! | Event     | Action   |
//! |-----------|----------|
//! | Startup   | PowerOn  |
//! | Shutdown  | PowerOff |
//! | Sleep     | PowerOff |
//! | Wake      | PowerOn  |
//! | Reboot    | None     |
//! | NoSession | None     |

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use tisvlay_test::PrivateBus;
use tisvlayd::tv_manager::SystemEvent;

/// Yield to the tokio executor repeatedly so the async pipeline
/// (mock subscription → forwarder → D-Bus) has time to flush.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Build a client against the shared private bus.
async fn client() -> DaemonClient {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    DaemonClient::from_connection(conn)
        .await
        .expect("daemon client")
}

// ---------- Shutdown → PowerOff ----------

#[tokio::test]
async fn shutdown_disconnects_connected_tv() {
    let tv_uuid = test_tv_uuid("lr_shutdown").to_string();
    let daemon = TestDaemon::with_one_mock_tv("lr_shutdown")
        .await
        .expect("daemon");

    // Connect the TV so it has a live MockConnection.
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    assert!(
        daemon.manager().is_connected(&tv_uuid).await,
        "TV should be connected after reconnect",
    );

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    // PowerOff drops the connection and marks powered_off.
    assert!(
        !daemon.manager().is_connected(&tv_uuid).await,
        "TV should be disconnected after shutdown",
    );
}

// ---------- Sleep → PowerOff ----------

#[tokio::test]
async fn sleep_disconnects_connected_tv() {
    let tv_uuid = test_tv_uuid("lr_sleep").to_string();
    let daemon = TestDaemon::with_one_mock_tv("lr_sleep")
        .await
        .expect("daemon");
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Sleep, "test")
        .await;
    settle().await;

    assert!(
        !daemon.manager().is_connected(&tv_uuid).await,
        "TV should be disconnected after sleep",
    );
}

// ---------- Reboot → None ----------

#[tokio::test]
async fn reboot_does_not_disconnect_tv() {
    let tv_uuid = test_tv_uuid("lr_reboot").to_string();
    let daemon = TestDaemon::with_one_mock_tv("lr_reboot")
        .await
        .expect("daemon");
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Reboot, "test")
        .await;
    settle().await;

    assert!(
        daemon.manager().is_connected(&tv_uuid).await,
        "TV should still be connected (reboot action is None by default)",
    );
}

// ---------- NoSession → None ----------

#[tokio::test]
async fn no_session_does_not_disconnect_tv() {
    let tv_uuid = test_tv_uuid("lr_nosess").to_string();
    let daemon = TestDaemon::with_one_mock_tv("lr_nosess")
        .await
        .expect("daemon");
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::NoSession, "test")
        .await;
    settle().await;

    assert!(
        daemon.manager().is_connected(&tv_uuid).await,
        "TV should still be connected (no_session action is None by default)",
    );
}

// ---------- D-Bus property pipeline ----------

#[tokio::test]
async fn reconnect_sets_connected_property_on_dbus() {
    let tv_uuid = test_tv_uuid("lr_dbus").to_string();
    let daemon = TestDaemon::with_one_mock_tv("lr_dbus")
        .await
        .expect("daemon");
    let client = client().await;

    // Initially disconnected.
    let tvs = client.list_tvs().await.expect("list_tvs");
    assert!(!tvs[0].connected, "starts disconnected");

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs after connect");
    assert!(tvs[0].connected, "D-Bus Connected property should be true");
}

#[tokio::test]
async fn shutdown_clears_connected_property_on_dbus() {
    let tv_uuid = test_tv_uuid("lr_dbus2").to_string();
    let daemon = TestDaemon::with_one_mock_tv("lr_dbus2")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs after shutdown");
    assert!(
        !tvs[0].connected,
        "D-Bus Connected should be false after shutdown",
    );
}
