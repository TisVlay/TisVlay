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

use tisvlay_core::{DaemonClient, PairTerminal};
use tisvlay_harness::daemon::{TestDaemon, mock_tv_config, test_tv_uuid};
use tisvlay_harness::pairing::wait_for_pair_phase;
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

    // Subscribe first so we capture the awaiting_tv phase as soon
    // as the daemon's spawned re-pair task reaches `start_pairing`.
    let stream = client
        .pairing_progress_stream()
        .await
        .expect("pairing progress stream");
    let mut stream = std::pin::pin!(stream);

    // Start re-pair against the mock TV. `pairing = "manual"`
    // (mock's default) means `start_pairing` waits for an explicit
    // cancel/submit command — the spawned task parks at
    // awaiting_tv. RPC returns Ok immediately (async-spawn shape).
    client
        .re_pair(&tv_uuid)
        .await
        .expect("re_pair RPC should succeed (sync validation)");

    // Wait for the awaiting_tv phase — confirms the spawned task
    // has reached `start_pairing` and registered its command sender
    // in `pairing_sessions` so CancelPair can find it.
    wait_for_pair_phase(stream.as_mut(), &tv_uuid, "awaiting_tv")
        .await
        .expect("daemon should reach awaiting_tv before cancel");

    // Cancel via Manager1.CancelPair. Must succeed since the pair
    // is in flight.
    client
        .cancel_pair(&tv_uuid)
        .await
        .expect("cancel_pair on in-flight session should succeed");

    // Drain to terminal. With the cancel propagating through, the
    // re-pair must terminate with `failed` carrying a cancel
    // message. We don't assert the exact wording — the cancel
    // ladder through PairingError → TvManagerError → DaemonError
    // may format it differently across versions. What matters is
    // that we reach `Failed` rather than `Complete`.
    let terminal =
        DaemonClient::drain_pair_to_terminal_pinned(stream.as_mut(), &tv_uuid, |_, _| {})
            .await
            .expect("drain should reach terminal");
    assert!(
        matches!(terminal, PairTerminal::Failed { .. }),
        "in-flight re_pair must reach Failed terminal after cancel; got {terminal:?}",
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
