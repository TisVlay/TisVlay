//! Regression: a second `Tv1.RePair()` call against a TV that's
//! already re-pairing is rejected with `PairingFailed("already
//! pairing")`. Gated by the `pairing_in_flight: bool` on
//! `ManagedTv` (set true at start of `re_pair_tv` under the
//! per-TV `Mutex`, cleared on completion).
//!
//! With Approach C's async-spawn shape, `re_pair_tv`'s synchronous
//! prelude claims the `pairing_in_flight` slot before spawning the
//! handshake. The RPC returns `Ok(())` immediately. The second call
//! sees `pairing_in_flight = true` synchronously and returns the
//! typed error — no race window between the two RPCs.
//!
//! Drives the test by starting a re-pair against a mock TV with
//! `pairing = "manual"` (default; no pre-seeded outcome) so the
//! handshake hangs waiting for an explicit user-accept signal.
//! Cleans up at the end via `Manager1.CancelPair` so the hung
//! handshake resolves before the test daemon drops.

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
async fn re_pair_blocks_when_already_pairing() {
    let config = mock_tv_config("block_test", true, None);
    let _daemon = TestDaemon::with_config(config).await.expect("test daemon");
    let client = client().await;
    let tv_uuid = test_tv_uuid("block_test").to_string();

    // Subscribe to PairingProgress so we can pause until the
    // spawned task has registered its command sender (which we'll
    // later cancel for cleanup).
    let stream = client
        .pairing_progress_stream()
        .await
        .expect("pairing progress stream");
    let mut stream = std::pin::pin!(stream);

    // First re-pair: returns Ok immediately. The sync prelude has
    // already claimed `pairing_in_flight = true` by the time this
    // returns — no race window.
    client
        .re_pair(&tv_uuid)
        .await
        .expect("first re_pair RPC should succeed");

    // Second re-pair: must return Err synchronously. The
    // `pairing_in_flight` check inside `re_pair_tv`'s sync prelude
    // sees the claimed slot and bails with `PairingFailed("already
    // pairing")`. No spawn happens for this call.
    let second = client.re_pair(&tv_uuid).await;
    let err = second.expect_err("second re_pair while first is in-flight must fail");
    let chain = format!("{:#}", err);
    assert!(
        chain.contains("already pairing"),
        "second re_pair error chain should mention 'already pairing'; got: {chain}",
    );

    // Wait until the daemon's spawned task reaches awaiting_tv so
    // the session command sender is registered in
    // `pairing_sessions` before we cancel.
    wait_for_pair_phase(stream.as_mut(), &tv_uuid, "awaiting_tv")
        .await
        .expect("daemon should reach awaiting_tv before cancel");

    // Clean up: cancel the in-flight first re-pair so its spawned
    // task resolves (with cancelled error) before the test daemon
    // drops. Drain to the terminal `failed` phase to confirm the
    // cancel propagates through.
    let _ = client.cancel_pair(&tv_uuid).await;
    let terminal =
        DaemonClient::drain_pair_to_terminal_pinned(stream.as_mut(), &tv_uuid, |_, _| {})
            .await
            .expect("drain should reach terminal");
    assert!(
        matches!(terminal, PairTerminal::Failed { .. }),
        "first re_pair must reach Failed terminal after cancel; got {terminal:?}",
    );
}
