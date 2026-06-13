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

// ---------- Reliable screen-state actions through system-event dispatch ----------

/// Build a config that maps a non-default `SessionAction` to `on_wake`
/// and `on_startup` on the "default" system profile. Used by the
/// reliable-action dispatch tests below to verify that
/// `execute_system_event` routes through the inherently-reliable
/// `screen_on` / `screen_off` paths in `tv_manager`.
fn config_with_profile_actions(
    tv_id: &str,
    on_wake: SessionAction,
    on_startup: SessionAction,
) -> SystemConfig {
    let mut config = mock_tv_config(tv_id, true, None);
    config.system.profiles.insert(
        tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: "default".to_string(),
            on_wake: Some(on_wake),
            on_startup: Some(on_startup),
            ..Default::default()
        },
    );
    config
}

/// `SystemEvent::Wake` with `on_wake = ScreenOn` short-circuits to
/// `power_on` when the TV's cached state is `"standby"`. Exercises the
/// system-event → `screen_on` → pre-flight-short-circuit path end to
/// end so the inherently-reliable plumbing actually runs through the
/// dispatcher (not just direct `manager.screen_on()` calls).
#[tokio::test]
async fn wake_with_screen_on_short_circuits() {
    let tv_uuid = test_tv_uuid("wake_scr_on").to_string();
    let config =
        config_with_profile_actions("wake_scr_on", SessionAction::ScreenOn, SessionAction::None);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    // Push cached PowerState to "standby" so the pre-flight short-
    // circuit fires inside `screen_on`.
    daemon
        .handle()
        .inject_power_state("mock://wake_scr_on", PowerState::Standby);
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Wake, "test")
        .await;

    // Short-circuit budget: power_on probe + WoL + reconnect +
    // initial subscription snapshot.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "active",
        "wake → screen_on with cached 'standby' should escalate via power_on",
    );
    assert!(
        tv.is_screen_on,
        "panel should be on after escalated power_on",
    );
}

/// `SystemEvent::Startup` with `on_startup = ScreenOff` powers the
/// TV on first when it's in `"standby"` — without the pre-flight
/// escalation, the screen_off command would never reach a live
/// connection. The user's intent ("park the TV in screen-off
/// substate at boot") requires power before screen, so this is the
/// observable contract.
#[tokio::test]
async fn on_startup_screen_off_powers_on_first() {
    let tv_uuid = test_tv_uuid("startup_scr_off").to_string();
    let config = config_with_profile_actions(
        "startup_scr_off",
        SessionAction::None,
        SessionAction::ScreenOff,
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    // Force cached PowerState to "standby" so the pre-flight branch
    // in `screen_off` has to escalate via power_on.
    daemon
        .handle()
        .inject_power_state("mock://startup_scr_off", PowerState::Standby);
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Startup, "test")
        .await;

    // Budget: power_on probe + WoL + reconnect + screen_off +
    // verify-delay.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert_eq!(
        tv.power_state, "screen_off",
        "startup → screen_off from 'standby' should power on first, then screen_off",
    );
    assert!(
        !tv.is_screen_on,
        "panel should be off after screen_off lands on the live connection",
    );
}

// ---------- Greeter idle dispatch (Manager1.NotifyGreeterIdle) ----------

/// Build a config whose "default" system profile sets the greeter idle
/// fields, so an automated TV inherits them.
fn config_with_greeter_idle(
    tv_id: &str,
    idle_timeout_seconds: u64,
    on_idle_timeout: SessionAction,
    on_idle_resume: SessionAction,
) -> SystemConfig {
    let mut config = mock_tv_config(tv_id, true, None);
    config.system.profiles.insert(
        tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: "default".to_string(),
            idle_timeout_seconds: Some(idle_timeout_seconds),
            on_idle_timeout: Some(on_idle_timeout),
            on_idle_resume: Some(on_idle_resume),
            ..Default::default()
        },
    );
    config
}

/// `NotifyGreeterIdle(true)` blanks the panel via `on_idle_timeout =
/// ScreenOff`; `NotifyGreeterIdle(false)` restores it via `on_idle_resume
/// = ScreenOn`. Exercises the full Manager1 method → `execute_system_event`
/// → reliable `screen_off`/`screen_on` path.
#[tokio::test]
async fn greeter_idle_blanks_then_resume_restores_screen() {
    let tv_uuid = test_tv_uuid("greeter_idle").to_string();
    let config = config_with_greeter_idle(
        "greeter_idle",
        600,
        SessionAction::ScreenOff,
        SessionAction::ScreenOn,
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    client
        .notify_greeter_idle(true)
        .await
        .expect("notify greeter idle");
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        !tv.is_screen_on,
        "greeter idle should blank the panel via screen_off",
    );

    client
        .notify_greeter_idle(false)
        .await
        .expect("notify greeter resume");
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.is_screen_on,
        "greeter resume should restore the panel via screen_on",
    );
}

/// `GreeterIdleTimeoutSeconds` reports the configured timeout for an
/// automated TV.
#[tokio::test]
async fn greeter_idle_timeout_reports_configured_value() {
    let config = config_with_greeter_idle(
        "greeter_timeout",
        600,
        SessionAction::ScreenOff,
        SessionAction::ScreenOn,
    );
    // Kept alive (registered on the shared bus) for the client call below.
    let _daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;

    let secs = client
        .greeter_idle_timeout_seconds()
        .await
        .expect("greeter idle timeout");
    assert_eq!(secs, 600, "configured idle timeout for the automated TV");
}

/// With no greeter idle timeout configured (hard-coded baseline = 0), the
/// daemon reports 0, so the greeter client leaves the watch disabled.
#[tokio::test]
async fn greeter_idle_timeout_zero_when_unset() {
    // Kept alive (registered on the shared bus) for the client call below.
    let _daemon = TestDaemon::with_one_mock_tv("greeter_unset")
        .await
        .expect("daemon");
    let client = client().await;

    let secs = client
        .greeter_idle_timeout_seconds()
        .await
        .expect("greeter idle timeout");
    assert_eq!(secs, 0, "no automated TV has a greeter idle timeout set");
}
