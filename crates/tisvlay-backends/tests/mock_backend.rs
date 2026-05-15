//! End-to-end integration test for the mock backend.
//!
//! Exercises the full flow a daemon goes through when a mock TV is in
//! the config:
//!
//! 1. Create a registry (with `mock` feature enabled)
//! 2. Look up the mock connector by vendor ID
//! 3. Pair (dummy flow)
//! 4. Connect
//! 5. Subscribe to state updates
//! 6. Mutate state via the trait methods
//! 7. Verify the subscriber receives the cumulative state on every change
//!
//! If this test passes, the daemon's state-change forwarder, D-Bus
//! property cache, and reconnection logic will all exercise real code
//! paths against the mock.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_backends::create_registry;
use tokio::time::timeout;

fn mock_config() -> toml::Value {
    toml::Value::try_from(toml::toml! {
        address = "mock://living_room"
        initial_power_state = "active"
        initial_volume = 30
        initial_input = "HDMI_1"
        initial_backlight = 80
        inputs = [
            ["HDMI_1", "HDMI 1"],
            ["HDMI_2", "HDMI 2"],
        ]
    })
    .unwrap()
}

#[tokio::test]
async fn mock_backend_registered() {
    let registry = create_registry();
    let backend = registry
        .get("mock")
        .expect("mock backend should be registered when the `mock` feature is enabled");
    assert_eq!(backend.backend.vendor_id(), "mock");
    assert_eq!(backend.backend.vendor_name(), "Mock (testing)");
}

#[tokio::test]
async fn mock_connector_transport_and_ip_semantics() {
    let registry = create_registry();
    let connector = registry.get_connector("mock").unwrap();

    // Transport must be Network so carrier/route events apply.
    assert!(connector.transport().is_network_dependent());
    // And must NOT survive IP changes, so NIC-failover tests work
    // against the same reconnect path as the LG backend.
    assert!(!connector.survives_source_ip_change());
}

#[tokio::test]
async fn mock_connect_and_mutate_state() {
    let registry = create_registry();
    let connector = registry.get_connector("mock").unwrap();
    let config = mock_config();

    let mut conn = connector.connect(&config).await.unwrap();
    assert!(conn.is_connected());

    // Initial state matches the config.
    let state = conn.get_state().await.unwrap();
    assert_eq!(state.volume, Some(30));
    assert_eq!(state.current_input.as_deref(), Some("HDMI_1"));
    assert_eq!(state.backlight, Some(80));

    // Mutations are reflected in subsequent state queries.
    conn.set_volume(55).await.unwrap();
    conn.switch_input("HDMI_2").await.unwrap();
    conn.set_backlight(40).await.unwrap();
    conn.set_mute(true).await.unwrap();

    let state = conn.get_state().await.unwrap();
    assert_eq!(state.volume, Some(55));
    assert_eq!(state.current_input.as_deref(), Some("HDMI_2"));
    assert_eq!(state.backlight, Some(40));
    assert_eq!(state.muted, Some(true));
}

#[tokio::test]
async fn mock_subscribe_pushes_state_on_every_change() {
    let registry = create_registry();
    let connector = registry.get_connector("mock").unwrap();
    let mut conn = connector.connect(&mock_config()).await.unwrap();

    let mut rx = conn.subscribe_state().await.unwrap();

    // Initial snapshot arrives immediately.
    let first = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("initial snapshot timeout")
        .expect("channel closed");
    assert_eq!(first.volume, Some(30));

    // Each mutation pushes a new snapshot.
    conn.set_volume(50).await.unwrap();
    let second = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("snapshot after set_volume timeout")
        .expect("channel closed");
    assert_eq!(second.volume, Some(50));

    conn.set_backlight(20).await.unwrap();
    let third = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("snapshot after set_backlight timeout")
        .expect("channel closed");
    assert_eq!(third.volume, Some(50));
    assert_eq!(third.backlight, Some(20));
}

#[tokio::test]
async fn mock_switch_input_rejects_unknown_id() {
    let registry = create_registry();
    let connector = registry.get_connector("mock").unwrap();
    let mut conn = connector.connect(&mock_config()).await.unwrap();

    let err = conn.switch_input("HDMI_99").await.unwrap_err();
    assert!(
        err.to_string().contains("unknown input"),
        "expected unknown-input error, got: {err}"
    );

    // Valid input still works.
    conn.switch_input("HDMI_2").await.unwrap();
}

#[tokio::test]
async fn mock_power_off_closes_connection() {
    let registry = create_registry();
    let connector = registry.get_connector("mock").unwrap();
    let mut conn = connector.connect(&mock_config()).await.unwrap();

    assert!(conn.is_connected());
    conn.power_off().await.unwrap();
    assert!(
        !conn.is_connected(),
        "power_off should close the connection"
    );
}
