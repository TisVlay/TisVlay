//! Multi-TV concurrency tests.
//!
//! Verifies that `execute_system_event` dispatches to each TV
//! independently — different profiles, different inputs, different
//! managed flags — without cross-talk.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::{SessionAction, SystemConfig, SystemProfileConfig};
use tisvlayd::test_harness::{mock_tv_config, TestDaemon};
use tisvlayd::tv_manager::SystemEvent;

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
        .reconnect("multi_managed")
        .await
        .expect("reconnect managed");
    daemon
        .manager()
        .reconnect("multi_unmanaged")
        .await
        .expect("reconnect unmanaged");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        !daemon.manager().is_connected("multi_managed").await,
        "managed TV should be disconnected after shutdown",
    );
    assert!(
        daemon.manager().is_connected("multi_unmanaged").await,
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
        "noop".to_string(),
        SystemProfileConfig {
            on_shutdown: Some(SessionAction::None),
            ..Default::default()
        },
    );

    // Assign the second TV to the "noop" profile.
    config.tv[1].profile.system = "noop".to_string();

    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    daemon
        .manager()
        .reconnect("mix_off")
        .await
        .expect("reconnect mix_off");
    daemon
        .manager()
        .reconnect("mix_noop")
        .await
        .expect("reconnect mix_noop");
    settle().await;

    daemon
        .manager()
        .execute_system_event(SystemEvent::Shutdown, "test")
        .await;
    settle().await;

    assert!(
        !daemon.manager().is_connected("mix_off").await,
        "TV with default profile should be powered off",
    );
    assert!(
        daemon.manager().is_connected("mix_noop").await,
        "TV with noop profile should remain connected",
    );
}
