//! Multi-TV concurrency tests.
//!
//! Verifies that `execute_system_event` dispatches to each TV
//! independently — different profiles, different inputs, different
//! managed flags — without cross-talk.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::{SessionAction, SystemConfig, SystemProfileConfig};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config, test_tv_uuid};
use tisvlayd::tv_manager::SystemEvent;
use uuid::Uuid;

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Build a config with two mock TVs, each configurable independently.
fn two_tv_config(id_a: &str, managed_a: bool, id_b: &str, managed_b: bool) -> SystemConfig {
    let mut config = mock_tv_config(id_a, managed_a, None);
    // Append the second TV.
    let second = mock_tv_config(id_b, managed_b, None);
    config.tv.extend(second.tv);
    config
}

#[tokio::test]
async fn shutdown_only_affects_managed_tvs() {
    let config = two_tv_config("multi_managed", true, "multi_unmanaged", false);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("multi_managed").to_string())
        .await
        .expect("reconnect managed");
    daemon
        .manager()
        .reconnect(&test_tv_uuid("multi_unmanaged").to_string())
        .await
        .expect("reconnect unmanaged");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        !daemon
            .manager()
            .is_connected(&test_tv_uuid("multi_managed").to_string())
            .await,
        "managed TV should be disconnected after shutdown",
    );
    assert!(
        daemon
            .manager()
            .is_connected(&test_tv_uuid("multi_unmanaged").to_string())
            .await,
        "unmanaged TV should remain connected after shutdown",
    );
}

#[tokio::test]
async fn mixed_profiles_dispatch_independently() {
    // TV A: default profile (shutdown → PowerOff)
    // TV B: custom profile (shutdown → None)
    let mut config = two_tv_config("mix_off", true, "mix_noop", true);

    // Override the default profile's on_shutdown to PowerOff (already default)
    // and create a "noop" profile that maps shutdown → None.
    config.system.profiles.insert(
        Uuid::new_v4(),
        SystemProfileConfig {
            name: "noop".to_string(),
            on_shutdown: Some(SessionAction::None),
            ..Default::default()
        },
    );

    // Assign the second TV to the "noop" profile.
    config.tv[1].profile.system = "noop".to_string();

    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect(&test_tv_uuid("mix_off").to_string())
        .await
        .expect("reconnect mix_off");
    daemon
        .manager()
        .reconnect(&test_tv_uuid("mix_noop").to_string())
        .await
        .expect("reconnect mix_noop");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        !daemon
            .manager()
            .is_connected(&test_tv_uuid("mix_off").to_string())
            .await,
        "TV with default profile should be powered off",
    );
    assert!(
        daemon
            .manager()
            .is_connected(&test_tv_uuid("mix_noop").to_string())
            .await,
        "TV with noop profile should remain connected",
    );
}
