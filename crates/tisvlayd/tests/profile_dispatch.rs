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
use tisvlay_core::{DaemonClient, PowerState, SessionAction, SystemConfig, SystemProfileConfig};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config, test_tv_uuid};
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

// ---------- Wake → reliable screen_on routes through dispatch_action ----------

/// Regression test for the system-event reliable-tier code path.
///
/// PR #56 (Action QoS) claims `tv_manager::execute_system_event`
/// funnels through `dispatch_action` with the resolved QoS so
/// configuring `on_wake = { action = "screen_on" }` gets the same
/// verify-retry-escalate semantics as a user-session unlock event.
/// The pre-existing tests in this file only cover sleep-side actions
/// (`Shutdown`, `Sleep`, `Reboot`, `NoSession`), which all bypass the
/// QoS branch in `dispatch_action`'s match. Without a wake-side test
/// a refactor that broke the system-event → `dispatch_action` →
/// `dispatch_reliable_screen_on` plumbing would slip through.
///
/// Setup: override the "default" system profile so `on_wake` resolves
/// to `screen_on` (instead of the built-in `power_on` default). The
/// asymmetric default makes that `(ScreenOn, Reliable)` — the only
/// `(action, qos)` pair `dispatch_action` actually branches on.
/// Inject `PowerState::Standby` so cached state reads `"standby"`
/// and the reliable-tier's pre-flight short-circuit fires, routing
/// straight to `power_on`, which reconnects with `initial_power_state
/// = "active"`. End-state assertion pins that the wake event drove
/// the TV back to `"active"` through this specific path.
#[tokio::test]
async fn wake_with_reliable_screen_on_short_circuits_via_dispatch_action() {
    let tv_id = "lr_wake_reliable";
    let tv_uuid = test_tv_uuid(tv_id).to_string();

    let mut config: SystemConfig = mock_tv_config(tv_id, true, None);
    config.system.profiles.insert(
        "default".to_string(),
        SystemProfileConfig {
            on_wake: Some(SessionAction::ScreenOn.into()),
            ..Default::default()
        },
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    // Push the TV past Screen Off so cached PowerState reads
    // "standby" — the predicate the reliable-tier short-circuit
    // matches on.
    daemon
        .handle()
        .inject_power_state(&format!("mock://{tv_id}"), PowerState::Standby);
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Wake, "test")
        .await;

    // Short-circuit → power_on probe → WoL → reconnect → new
    // MockConnection with default initial_power_state = "active".
    // Generous wall-clock budget; the short-circuit path itself is
    // sub-second but the new-connection state push needs a tick.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = client().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "Wake event with on_wake = screen_on should route through \
         dispatch_action's reliable-tier short-circuit and land the TV \
         in active state via power_on escalation",
    );
    assert!(
        tv.is_screen_on,
        "screen should be on after the escalated power_on completes",
    );
}
