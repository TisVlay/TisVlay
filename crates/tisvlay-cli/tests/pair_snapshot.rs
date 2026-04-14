//! CLI `tisvlay pair` output snapshot tests.
//!
//! Spawns a `TestDaemon` on a private `dbus-daemon`, then runs the
//! `tisvlay` CLI binary as a subprocess with `DBUS_SESSION_BUS_ADDRESS`
//! pointing at the test bus. The stdout/stderr output is snapshotted
//! via `insta-cmd` so regressions in user-facing messages are caught.

#![cfg(feature = "mock")]

use insta_cmd::assert_cmd_snapshot;
use std::process::Command;
use tisvlay_core::DaemonClient;
use tisvlay_test::PrivateBus;
use tisvlayd::test_harness::TestDaemon;

/// Path to the compiled `tisvlay` binary under the test profile.
fn cli_bin() -> std::path::PathBuf {
    // `env!("CARGO_BIN_EXE_tisvlay")` is only available in the same
    // package's integration tests. Since tisvlay-cli defines the binary,
    // and we ARE in tisvlay-cli's integration tests, it works here.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_tisvlay"))
}

/// Spin up an empty daemon on the private bus and return a
/// `(daemon, bus_address)` pair. The daemon has no pre-configured TVs.
async fn empty_daemon_on_bus() -> (TestDaemon, String) {
    let config = tisvlay_core::SystemConfig::default();
    let daemon = TestDaemon::with_config(config).await.unwrap();
    let address = PrivateBus::shared().address().to_string();
    (daemon, address)
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_happy_path_output() {
    let (daemon, bus_addr) = empty_daemon_on_bus().await;
    daemon.handle().simulate_user_accept("mock://living_room");

    assert_cmd_snapshot!(Command::new(cli_bin())
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .arg("--user")
        .arg("pair")
        .arg("mock://living_room")
        .arg("--vendor")
        .arg("mock")
        .arg("--name")
        .arg("Living Room"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_with_wake_id_output() {
    let (daemon, bus_addr) = empty_daemon_on_bus().await;
    daemon.handle().simulate_user_accept("mock://living_room");

    assert_cmd_snapshot!(Command::new(cli_bin())
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .arg("--user")
        .arg("pair")
        .arg("mock://living_room")
        .arg("--vendor")
        .arg("mock")
        .arg("--name")
        .arg("Living Room")
        .arg("--wake-id")
        .arg("AA:BB:CC:DD:EE:FF"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_rejected_output() {
    let (daemon, bus_addr) = empty_daemon_on_bus().await;
    daemon.handle().simulate_user_reject("mock://denied");

    assert_cmd_snapshot!(Command::new(cli_bin())
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .arg("--user")
        .arg("pair")
        .arg("mock://denied")
        .arg("--vendor")
        .arg("mock")
        .arg("--name")
        .arg("Denied TV"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_without_name_uses_address() {
    let (daemon, bus_addr) = empty_daemon_on_bus().await;
    daemon.handle().simulate_user_accept("mock://office");

    assert_cmd_snapshot!(Command::new(cli_bin())
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .arg("--user")
        .arg("pair")
        .arg("mock://office")
        .arg("--vendor")
        .arg("mock"));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_after_pair_output() {
    let (daemon, bus_addr) = empty_daemon_on_bus().await;
    daemon.handle().simulate_user_accept("mock://bedroom");

    // Pair first (via D-Bus client directly — we're testing list output, not pair again).
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .unwrap();
    let config_json = r#"{"address": "mock://bedroom"}"#;
    client
        .pair_tv("Bedroom", "mock", config_json, true)
        .await
        .unwrap();

    assert_cmd_snapshot!(Command::new(cli_bin())
        .env("DBUS_SESSION_BUS_ADDRESS", &bus_addr)
        .arg("--user")
        .arg("list"));
}
