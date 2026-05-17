//! Integration tests for the automation gate composition:
//!
//!   managed × automate_when × primary_input × current_input
//!
//! `execute_system_event` only fires actions on TVs where `is_automated()`
//! returns true. This file exercises the four inputs that compose it.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::{
    AutomateWhen, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SessionAction, SystemConfig,
    SystemProfileConfig,
};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config, test_tv_uuid};
use tisvlayd::tv_manager::SystemEvent;

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

// ---------- managed=false skips automation ----------

#[tokio::test]
async fn unmanaged_tv_is_not_automated() {
    let config = mock_tv_config("gate_unmanaged", false, None);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("gate_unmanaged").to_string())
        .await
        .expect("reconnect");
    settle().await;

    // Shutdown maps to PowerOff, but the TV is unmanaged → skip.
    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        daemon
            .manager()
            .is_connected(&test_tv_uuid("gate_unmanaged").to_string())
            .await,
        "unmanaged TV should remain connected after shutdown event",
    );
}

// ---------- automate_when=on_primary ----------

fn on_primary_config(tv_id: &str, primary_input: &str) -> SystemConfig {
    let mut config = mock_tv_config(tv_id, true, Some(primary_input));
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_shutdown: Some(SessionAction::PowerOff),
            automate_when: Some(AutomateWhen::OnPrimary),
            ..Default::default()
        },
    );
    config
}

#[tokio::test]
async fn on_primary_fires_when_input_matches() {
    let config = on_primary_config("gate_onp_match", "HDMI_1");
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("gate_onp_match").to_string())
        .await
        .expect("reconnect");
    settle().await;

    // The mock connection starts with current_input="HDMI_1" (the
    // default from MockVendorConfig). So on_primary matches.
    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        !daemon
            .manager()
            .is_connected(&test_tv_uuid("gate_onp_match").to_string())
            .await,
        "on_primary should fire when current_input == primary_input",
    );
}

#[tokio::test]
async fn on_primary_skips_when_input_differs() {
    // primary_input is HDMI_2, but the mock starts on HDMI_1.
    let config = on_primary_config("gate_onp_skip", "HDMI_2");
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("gate_onp_skip").to_string())
        .await
        .expect("reconnect");
    settle().await;

    // Wait for the mock subscription to push current_input.
    // The mock pushes its initial state on subscribe_state.
    tokio::time::sleep(Duration::from_millis(100)).await;
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        daemon
            .manager()
            .is_connected(&test_tv_uuid("gate_onp_skip").to_string())
            .await,
        "on_primary should skip when current_input != primary_input",
    );
}

// ---------- automate_when=off_primary ----------

fn off_primary_config(tv_id: &str, primary_input: &str) -> SystemConfig {
    let mut config = mock_tv_config(tv_id, true, Some(primary_input));
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_shutdown: Some(SessionAction::PowerOff),
            automate_when: Some(AutomateWhen::OffPrimary),
            ..Default::default()
        },
    );
    config
}

#[tokio::test]
async fn off_primary_fires_when_input_differs() {
    // primary_input is HDMI_2, mock is on HDMI_1 → off-primary fires.
    let config = off_primary_config("gate_offp_fire", "HDMI_2");
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("gate_offp_fire").to_string())
        .await
        .expect("reconnect");
    settle().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        !daemon
            .manager()
            .is_connected(&test_tv_uuid("gate_offp_fire").to_string())
            .await,
        "off_primary should fire when current_input != primary_input",
    );
}

#[tokio::test]
async fn off_primary_skips_when_input_matches() {
    // primary_input is HDMI_1, mock is on HDMI_1 → off-primary skips.
    let config = off_primary_config("gate_offp_skip", "HDMI_1");
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("gate_offp_skip").to_string())
        .await
        .expect("reconnect");
    settle().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        daemon
            .manager()
            .is_connected(&test_tv_uuid("gate_offp_skip").to_string())
            .await,
        "off_primary should skip when current_input == primary_input",
    );
}
