//! Regression: `Manager1.CancelPair(tv_id)` aborts an in-flight
//! pairing handshake. The `pairing_sessions` map on `TvManager`
//! holds a clone of the session's command channel; cancel sends
//! `PairingCommand::Cancel` via that channel, which the session's
//! `wait_for_completion` resolves with `PairingError::cancelled`.
//!
//! Bonus assertion: `Manager1.CancelPair` against a tv_id that
//! isn't currently pairing returns `NotPairing` — strict variant
//! matching the BlueZ `Adapter.RemoveDevice`-on-unknown-device
//! convention.

#![cfg(feature = "mock")]

use std::time::Duration;

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
async fn cancel_pair_aborts_in_flight_re_pair() {
    let config = mock_tv_config("cancel_test", true, None);
    let _daemon = TestDaemon::with_config(config).await.expect("test daemon");
    let client = client().await;
    let tv_uuid = test_tv_uuid("cancel_test").to_string();

    // Start a re-pair against the mock TV in `pairing = "manual"`
    // mode (the default): `start_pairing` waits for an explicit
    // cancel or submit command, so this call hangs.
    let client_a = client.clone();
    let tv_a = tv_uuid.clone();
    let pair_task = tokio::spawn(async move { client_a.re_pair(&tv_a).await });

    // Settle window so the re-pair task has reached the
    // `start_pairing` await point and the session's command sender
    // is registered in `pairing_sessions` before we try to cancel.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cancel via Manager1.CancelPair. Must succeed since the
    // pair is in flight.
    client
        .cancel_pair(&tv_uuid)
        .await
        .expect("cancel_pair on in-flight session should succeed");

    // The re-pair task should resolve with an error (cancelled).
    // We don't assert the exact message — the cancel ladder
    // through PairingError → TvManagerError → DaemonError may
    // format it differently across versions. What matters is
    // termination.
    let pair_result = pair_task.await.expect("pair task joins");
    assert!(
        pair_result.is_err(),
        "in-flight re_pair must resolve with an error after cancel; got Ok",
    );
}

#[tokio::test]
async fn cancel_pair_returns_not_pairing_for_unknown_tv() {
    let _daemon = TestDaemon::with_one_mock_tv("not_pairing_test")
        .await
        .expect("test daemon");
    let client = client().await;
    let known_uuid = test_tv_uuid("not_pairing_test").to_string();

    // Case 1: cancel against the known TV's uuid when no pair is
    // in flight. The TV exists in the daemon but no pair-session
    // is registered for it.
    let err = client
        .cancel_pair(&known_uuid)
        .await
        .expect_err("cancel_pair without in-flight session must fail");
    let chain = format!("{:#}", err);
    assert!(
        chain.contains("not currently pairing") || chain.contains("NotPairing"),
        "cancel_pair on a non-pairing TV must return NotPairing; got: {chain}",
    );

    // Case 2: cancel against a completely unknown tv_id. Same
    // NotPairing path — the map lookup misses regardless of
    // whether the tv_id matches a known TV.
    let unknown = "00000000-0000-0000-0000-000000000000".to_string();
    let err = client
        .cancel_pair(&unknown)
        .await
        .expect_err("cancel_pair on unknown tv_id must fail");
    let chain = format!("{:#}", err);
    assert!(
        chain.contains("not currently pairing") || chain.contains("NotPairing"),
        "cancel_pair on unknown tv_id must return NotPairing; got: {chain}",
    );
}
