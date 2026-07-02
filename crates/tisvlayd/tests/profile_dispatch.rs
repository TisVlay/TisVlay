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

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::{
    DaemonClient, PowerState, SessionAction, SystemConfig, SystemProfileConfig,
    power_state_str_is_on,
};
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

#[tokio::test]
async fn power_off_clears_connected_property_on_poll_only_tv() {
    // Regression: a poll-only backend (e.g. a projector over a
    // request/response transport) has no push subscription task, so nothing
    // emits `ConnectionChanged` when `power_off` drops the connection.
    // Without an explicit emit the cached D-Bus `Connected` property — and
    // the GUI power toggle keyed on it — stays stuck at `true`, leaving the
    // TV un-powerable-on from that button. The push-mock equivalent above
    // passes via the subscription-end path; this exercises the poll-only
    // path. (CEC is a *push* backend — its follower-mode monitor takes the
    // subscription-end path, covered by the tisvlay-cec crate tests.)
    let tv_uuid = test_tv_uuid("lr_poll_off").to_string();
    let daemon = TestDaemon::with_one_poll_only_mock_tv("lr_poll_off")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs after connect");
    assert!(tvs[0].connected, "poll-only TV connects");

    daemon
        .manager()
        .power_off(&tv_uuid)
        .await
        .expect("power_off");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs after power_off");
    assert!(
        !tvs[0].connected,
        "D-Bus Connected must be false after power_off on a poll-only TV",
    );
}

#[tokio::test]
async fn power_off_keeps_connection_for_reachable_in_standby_tv() {
    // A CEC-style backend stays reachable in standby: power_off keeps the
    // connection (Connected stays true) and only moves the *power* axis to
    // standby — unlike a network backend whose socket dies on power-off.
    // This is what makes button-off match remote-off for CEC.
    let tv_uuid = test_tv_uuid("lr_ris").to_string();
    let daemon = TestDaemon::with_one_reachable_in_standby_mock_tv("lr_ris")
        .await
        .expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs after connect");
    assert!(tvs[0].connected, "connects");

    daemon
        .manager()
        .power_off(&tv_uuid)
        .await
        .expect("power_off");
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs after power_off");
    assert!(
        tvs[0].connected,
        "a reachable-in-standby TV stays Connected after power_off (D-Bus)",
    );
    // ...and the live connection is genuinely kept (not just the cached
    // property), matching CecConnection which no longer clears connected.
    assert!(
        daemon.manager().is_connected(&tv_uuid).await,
        "the underlying connection is kept, not merely the cached property",
    );
    assert!(
        !power_state_str_is_on(&tvs[0].power_state),
        "power reflects standby",
    );
}

#[tokio::test]
async fn power_on_is_a_noop_when_already_connected_and_on() {
    // Regression: a reachable-in-standby TV (CEC-style) whose connection
    // persists across standby must treat a redundant power_on — e.g. an
    // `on_lock_wake` profile action firing once per idle cycle while locked
    // — as a no-op, NOT a teardown+reconnect. Without the idempotent fast
    // path in `power_on`, each redundant call flapped Connected
    // (disconnect→reconnect), storming the tray's connect/disconnect
    // notifications.
    let tv_uuid = test_tv_uuid("lr_ris_idem").to_string();
    let addr = "mock://lr_ris_idem";
    let daemon = TestDaemon::with_one_reachable_in_standby_mock_tv("lr_ris_idem")
        .await
        .expect("daemon");

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;
    assert!(daemon.manager().is_connected(&tv_uuid).await, "connects");
    // The mock reports "active" by default, so the subscription push has
    // seeded the cached power state as on. Baseline the connect count here
    // (robust to any boot-time connect) and assert power_on doesn't add one.
    let before = daemon.handle().connect_call_count(addr);

    daemon.manager().power_on(&tv_uuid).await.expect("power_on");
    settle().await;
    assert_eq!(
        daemon.handle().connect_call_count(addr),
        before,
        "power_on on an already-connected, already-on TV is a no-op (no reconnect flap)",
    );
    assert!(
        daemon.manager().is_connected(&tv_uuid).await,
        "the live connection is kept across the redundant power_on",
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

/// A TV with idle_timeout_seconds=0 (greeter idle disabled) must NOT be
/// blanked when GreeterIdle fires, even though its on_idle_timeout defaults
/// to ScreenOff — otherwise an un-opted-in TV would blank whenever any other
/// TV's greeter timeout elapses.
#[tokio::test]
async fn greeter_idle_skips_tv_with_zero_timeout() {
    let tv_uuid = test_tv_uuid("greeter_zero").to_string();
    let config = config_with_greeter_idle(
        "greeter_zero",
        0, // disabled for this TV
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
        tv.is_screen_on,
        "a TV with idle_timeout_seconds=0 must not be blanked by greeter idle",
    );
}

/// A greeter resume (e.g. the client's startup presence-assert) must re-light
/// an automated TV the prior session latched dark, *even when greeter idle is
/// disabled for it* (idle_timeout_seconds=0). Only proactive blanking is opt-in;
/// restoring presence at the login screen is not gated on the idle timer.
#[tokio::test]
async fn greeter_resume_lights_tv_with_zero_timeout() {
    let tv_uuid = test_tv_uuid("greeter_resume_zero").to_string();
    let config = config_with_greeter_idle(
        "greeter_resume_zero",
        0, // greeter idle disabled for this TV
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

    // Simulate a prior locked session that latched the panel dark across logout.
    daemon
        .manager()
        .screen_off(&tv_uuid)
        .await
        .expect("screen_off");
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(!tv.is_screen_on, "precondition: panel latched off");

    // The greeter's presence-assert / resume edge.
    client
        .notify_greeter_idle(false)
        .await
        .expect("notify greeter resume");
    settle().await;
    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.is_screen_on,
        "greeter resume must re-light an automated TV even with idle disabled",
    );
}

/// A TV with a timeout but no idle action (on_idle_timeout=None) must not
/// govern the single greeter clock — it would otherwise pull the shared
/// timeout shorter while never acting.
#[tokio::test]
async fn greeter_idle_timeout_ignores_actionless_tv() {
    let config = config_with_greeter_idle(
        "greeter_noaction",
        600,
        SessionAction::None, // timeout set, but nothing to do at idle
        SessionAction::ScreenOn,
    );
    let _daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = client().await;

    let secs = client
        .greeter_idle_timeout_seconds()
        .await
        .expect("greeter idle timeout");
    assert_eq!(
        secs, 0,
        "a TV with no idle action must not govern the greeter clock",
    );
}

// ---------- Desired-screen latch + push-driven reconcile ----------

/// `reconcile_screen` (driven by the state-change forwarder on every push)
/// re-asserts the latched `desired_screen`: if the panel drifts off while the
/// desire is "on", the next state push restores it — with no `power_on`
/// escalation (the recursion-proof re-assert path).
#[tokio::test]
async fn reconcile_reasserts_desired_on_after_drift() {
    let id = "reconcile_reassert";
    let tv_uuid = test_tv_uuid(id).to_string();
    let addr = "mock://reconcile_reassert";
    let daemon = TestDaemon::with_one_mock_tv(id).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    // Latch desired = on.
    daemon
        .manager()
        .screen_on(&tv_uuid)
        .await
        .expect("screen_on");
    settle().await;

    // Panel drifts off (firmware slipped past the lit state). The push
    // triggers reconcile, which re-asserts desired=on via a bare screen_on.
    daemon
        .handle()
        .inject_power_state(addr, PowerState::ScreenOff);
    settle().await;
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.is_screen_on,
        "reconcile should re-assert desired=on after the panel drifted off",
    );
}

/// A `screen_off` issued while a lifecycle wake is in progress
/// (`is_reconnecting`) defers: it records `desired_screen` and returns without
/// sending a `turnOffScreen` that would race the wake. The push-driven
/// reconcile applies it once the TV is reachable again (the cold-boot tangle
/// fix).
#[tokio::test]
async fn screen_off_deferred_during_reconnect_applies_on_ready() {
    let id = "defer_reconnect";
    let tv_uuid = test_tv_uuid(id).to_string();
    let addr = "mock://defer_reconnect";
    let daemon = TestDaemon::with_one_mock_tv(id).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;
    daemon.handle().inject_power_state(addr, PowerState::Active);
    settle().await;

    // Make the next connect slow, then kick a reconnect so the TV sits in
    // `reconnecting` long enough to issue a screen action during the window.
    daemon
        .handle()
        .set_connect_delay(Duration::from_millis(700));
    let mgr = daemon.manager().clone();
    let reconnect_uuid = tv_uuid.clone();
    let reconnect_task = tokio::spawn(async move {
        let _ = mgr.reconnect(&reconnect_uuid).await;
    });

    // Wait until the reconnect has set `reconnecting`.
    let mut waited = Duration::ZERO;
    while !daemon.manager().is_reconnecting(&tv_uuid).await {
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += Duration::from_millis(10);
        assert!(
            waited < Duration::from_millis(600),
            "reconnect never entered the reconnecting state",
        );
    }

    let before = daemon.handle().screen_off_call_count(addr);
    daemon
        .manager()
        .screen_off(&tv_uuid)
        .await
        .expect("screen_off (deferred) returns Ok");
    assert!(
        daemon.manager().is_reconnecting(&tv_uuid).await,
        "still reconnecting when screen_off was issued",
    );
    assert_eq!(
        daemon.handle().screen_off_call_count(addr),
        before,
        "screen_off must defer (send no turnOffScreen) while reconnecting",
    );

    // Let the reconnect finish; the post-wake push drives reconcile, which
    // applies the deferred screen_off.
    daemon.handle().clear_connect_delay();
    let _ = reconnect_task.await;
    settle().await;
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        !tv.is_screen_on,
        "deferred screen_off should be applied by reconcile once reachable",
    );
}

/// Latest-wins: an off-then-on pair issued while the TV is reconnecting both
/// defer (latching `desired_screen`), and reconcile applies only the latest
/// (on) once the TV is reachable — the cross-login supersession case (a login
/// `screen_on` superseding a stale greeter `screen_off`).
#[tokio::test]
async fn latest_wins_off_then_on_while_reconnecting() {
    let id = "latest_wins";
    let tv_uuid = test_tv_uuid(id).to_string();
    let addr = "mock://latest_wins";
    let daemon = TestDaemon::with_one_mock_tv(id).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;
    daemon.handle().inject_power_state(addr, PowerState::Active);
    settle().await;

    daemon
        .handle()
        .set_connect_delay(Duration::from_millis(700));
    let mgr = daemon.manager().clone();
    let reconnect_uuid = tv_uuid.clone();
    let reconnect_task = tokio::spawn(async move {
        let _ = mgr.reconnect(&reconnect_uuid).await;
    });

    let mut waited = Duration::ZERO;
    while !daemon.manager().is_reconnecting(&tv_uuid).await {
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += Duration::from_millis(10);
        assert!(
            waited < Duration::from_millis(600),
            "reconnect never entered the reconnecting state",
        );
    }

    // Both defer; the latch ends as `on`.
    daemon
        .manager()
        .screen_off(&tv_uuid)
        .await
        .expect("screen_off deferred");
    daemon
        .manager()
        .screen_on(&tv_uuid)
        .await
        .expect("screen_on deferred");

    daemon.handle().clear_connect_delay();
    let _ = reconnect_task.await;
    settle().await;
    settle().await;

    // The mock re-inits to Active (panel on) on reconnect, which would let the
    // final assert pass vacuously. Force the panel dark, so re-lighting it can
    // only come from reconcile applying the latched *on* (the latest intent);
    // had the latch wrongly ended off, reconcile would leave it dark.
    daemon
        .handle()
        .inject_power_state(addr, PowerState::ScreenOff);
    settle().await;
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.is_screen_on,
        "latest-wins: the on should supersede the earlier off after reconnect",
    );
}

/// An explicit `Tv1.Power.PowerOn` overrides a stale `screen_off` latch: it sets
/// `desired_screen = Some(true)` before waking, so the push-driven reconcile
/// keeps the freshly-woken panel on instead of re-blanking it.
#[tokio::test]
async fn power_on_clears_stale_screen_off_latch() {
    let id = "poweron_clears_latch";
    let tv_uuid = test_tv_uuid(id).to_string();
    let addr = "mock://poweron_clears_latch";
    let daemon = TestDaemon::with_one_mock_tv(id).await.expect("daemon");
    let client = client().await;

    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    // Latch the panel off (a prior idle / lock blank).
    daemon
        .manager()
        .screen_off(&tv_uuid)
        .await
        .expect("screen_off");
    settle().await;

    // Explicit power-on must override that stale screen-off latch.
    client.power_on(&tv_uuid).await.expect("power_on");
    settle().await;

    // Panel drifts off again: with the latch correctly flipped to ON, reconcile
    // re-lights it; had the stale OFF latch survived the power-on, reconcile
    // would leave it dark.
    daemon
        .handle()
        .inject_power_state(addr, PowerState::ScreenOff);
    settle().await;
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.is_screen_on,
        "explicit power-on must flip the latch on; reconcile then keeps the panel lit",
    );
}
