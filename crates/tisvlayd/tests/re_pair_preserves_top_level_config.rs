//! Regression: `Tv1.RePair()` refreshes vendor credentials without
//! touching top-level config fields (`name`, `managed`,
//! `system_profile`, `primary_input`). The whole point of re-pair
//! is to recover from credential expiry without forcing the user to
//! re-enter all the per-TV setup they did at AddTv time.
//!
//! Drives a mock TV with `pairing = "manual"` (default) plus a
//! pre-seeded `simulate_user_accept` so `start_pairing` resolves
//! immediately with success. Reads the TV's config via
//! `Tv1.GetConfig` before and after re-pair, asserts the top-level
//! fields are byte-identical.

#![cfg(feature = "mock")]

use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config, test_tv_uuid};
use tisvlay_test::PrivateBus;

async fn client() -> DaemonClient {
    let bus_conn = PrivateBus::shared().connect().await.expect("client conn");
    DaemonClient::from_connection(bus_conn)
        .await
        .expect("daemon client")
}

#[tokio::test]
async fn re_pair_preserves_top_level_config() {
    let mut config = mock_tv_config("repair_test", true, Some("HDMI_2"));
    // Customize the name so we have a distinctive top-level field
    // beyond what `mock_tv_config`'s defaults provide. `mock_tv_config`
    // defaults `name` to the tv_id; overriding here gives us something
    // we can assert is preserved end-to-end.
    if let Some(tv) = config.tv.first_mut() {
        tv.name = "Living Room G4".to_string();
    }

    let daemon = TestDaemon::with_config(config).await.expect("test daemon");
    let handle = daemon.handle();
    let client = client().await;
    let tv_uuid = test_tv_uuid("repair_test").to_string();

    // Read the top-level fields before re-pair so we have a known
    // baseline to assert against.
    let before_json = client.get_tv_config(&tv_uuid).await.expect("get_config");
    let before: serde_json::Value = serde_json::from_str(&before_json).expect("parse before json");

    // Pre-seed an "accept" outcome on the mock so `start_pairing`
    // resolves immediately with success — the test doesn't need to
    // drive a manual handshake here, just verify the post-re-pair
    // top-level fields are intact.
    handle.simulate_user_accept("mock://repair_test");

    client
        .re_pair(&tv_uuid)
        .await
        .expect("re_pair should succeed");

    let after_json = client.get_tv_config(&tv_uuid).await.expect("get_config");
    let after: serde_json::Value = serde_json::from_str(&after_json).expect("parse after json");

    // Top-level fields must be byte-identical pre- and post-re-pair.
    // Use `assert_eq!` on each field individually so a failure shows
    // exactly which field changed.
    assert_eq!(
        before["name"], after["name"],
        "re-pair must preserve `name`",
    );
    assert_eq!(
        before["managed"], after["managed"],
        "re-pair must preserve `managed`",
    );
    assert_eq!(
        before["system_profile"], after["system_profile"],
        "re-pair must preserve `system_profile`",
    );
    assert_eq!(
        before["primary_input"], after["primary_input"],
        "re-pair must preserve `primary_input`",
    );
}
