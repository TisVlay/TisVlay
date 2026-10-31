//! The TV remote's keys: the system profile's `remote_keys` reaches each
//! live connection that can pass them, on connect and on every change.

use crate::support;

use std::time::Duration;

use support::*;

/// Wait until the mock's last `set_remote_keys` for `address` is `want`.
async fn wait_remote_keys(mock: &tisvlay_mock::MockHandle, address: &str, want: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while mock.remote_keys(address) != Some(want) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "remote keys never became {want}: {:?}",
            mock.remote_keys(address)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn keys_profile(on: bool) -> tisvlay_core::SystemProfileConfig {
    tisvlay_core::SystemProfileConfig {
        name: format!("Remote keys {on}"),
        remote_keys: Some(on),
        ..Default::default()
    }
}

/// A profile turning the keys on reaches the connection, and turning them
/// back off reaches it too.
#[tokio::test]
async fn the_profile_switches_the_remote_keys() {
    let (mut effects, client, mock) = spawn_reactor();
    let tv = add_connected_tv(&mut effects, &client, "mock://keys").await;

    assign_system_profile(&mut effects, &client, tv, keys_profile(true)).await;
    wait_remote_keys(&mock, "mock://keys", true).await;
    assign_system_profile(&mut effects, &client, tv, keys_profile(false)).await;
    wait_remote_keys(&mock, "mock://keys", false).await;
}

/// The stock profile leaves a new connection's claim as it is: nothing to
/// send.
#[tokio::test]
async fn the_stock_profile_sends_nothing() {
    let (mut effects, client, mock) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://keys-stock").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.remote_keys("mock://keys-stock"), None);
}

/// An unmanaged TV's claim goes without the keys, whatever its profile
/// says, and gets them back when it's managed again.
#[tokio::test]
async fn an_unmanaged_tv_gets_no_remote_keys() {
    let (mut effects, client, mock) = spawn_reactor();
    let tv = add_connected_tv(&mut effects, &client, "mock://keys-unmanaged").await;
    assign_system_profile(&mut effects, &client, tv, keys_profile(true)).await;
    wait_remote_keys(&mock, "mock://keys-unmanaged", true).await;

    set_managed(&mut effects, &client, tv, false).await;
    wait_remote_keys(&mock, "mock://keys-unmanaged", false).await;
    set_managed(&mut effects, &client, tv, true).await;
    wait_remote_keys(&mock, "mock://keys-unmanaged", true).await;
}
