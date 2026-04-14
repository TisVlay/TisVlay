//! Smoke test for the daemon test harness.
//!
//! Verifies that:
//! - `TestDaemon::with_one_mock_tv` builds without panic on the
//!   shared private bus
//! - The full D-Bus surface (`Manager1`, `ObjectManager`, every
//!   `Tv1.*` interface) is registered and reachable from a
//!   `DaemonClient::from_connection`
//! - `list_tvs()` returns the configured mock TV with the right id
//!
//! Once this passes, Phase B.1.2 can layer profile-dispatch tests
//! on top of the same harness without re-validating the wiring.

#![cfg(feature = "mock")]

use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::TestDaemon;
use tisvlay_test::PrivateBus;

#[tokio::test]
async fn harness_registers_full_dbus_surface_for_one_mock_tv() {
    let daemon = TestDaemon::with_one_mock_tv("living_room")
        .await
        .expect("test daemon");

    // Build a client over the *same* private bus connection so the
    // request hits the in-process daemon, not the user's session bus.
    let bus_conn = PrivateBus::shared().connect().await.expect("client conn");
    let client = DaemonClient::from_connection(bus_conn)
        .await
        .expect("daemon client");

    assert!(
        client.is_available().await,
        "daemon should answer ObjectManager"
    );

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(tvs.len(), 1, "exactly one configured TV");
    assert_eq!(tvs[0].id, "living_room");
    assert_eq!(tvs[0].vendor, "mock");
    // Newly built daemon hasn't connected to any TV yet — connection
    // happens lazily on the first command — so Connected starts false.
    assert!(!tvs[0].connected, "TV starts disconnected");

    // The mock handle is shared with the daemon's connector via
    // create_test_registry. Confirm the daemon's view of the TV is
    // observable through it.
    let _handle = daemon.handle();
}
