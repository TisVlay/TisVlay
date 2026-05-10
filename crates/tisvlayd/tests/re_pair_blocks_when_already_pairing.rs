//! Regression: a second `Tv1.RePair()` call against a TV that's
//! already re-pairing is rejected with `PairingFailed("already
//! pairing")`. Gated by the `pairing_in_flight: bool` on
//! `ManagedTv` (set true at start of `re_pair_tv` under the
//! per-TV `Mutex`, cleared on completion).
//!
//! Drives the test by starting a re-pair against a mock TV with
//! `pairing = "manual"` (default; no pre-seeded outcome) so the
//! handshake hangs waiting for an explicit user-accept signal.
//! While that's hanging, fires a second re-pair; expects the
//! second to return immediately with the typed "already pairing"
//! error. Cleans up at the end via `Manager1.CancelPair` so the
//! hung handshake resolves before the test daemon drops.

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
async fn re_pair_blocks_when_already_pairing() {
    let config = mock_tv_config("block_test", true, None);
    let _daemon = TestDaemon::with_config(config).await.expect("test daemon");
    let client = client().await;
    let tv_uuid = test_tv_uuid("block_test").to_string();

    // Start the first re-pair in a background task. The mock's
    // `pairing = "manual"` shorthand (the default) means
    // `start_pairing` waits for an explicit cancel or submit
    // command, so this call hangs until we cancel it below.
    let client_a = client.clone();
    let tv_a = tv_uuid.clone();
    let first = tokio::spawn(async move { client_a.re_pair(&tv_a).await });

    // Settle window so the first re-pair has time to reach the
    // `pairing_in_flight = true` claim before we fire the second.
    // Without this, the two re-pair calls would race and either
    // could win, defeating the test's purpose. Real-time sleep is
    // acceptable here per the established 50ms pattern in
    // `reload_resolves_system_profile.rs` — we're not asserting on
    // timing, just letting the first call's per-TV-Mutex acquire
    // happen-before the second.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let second = client.re_pair(&tv_uuid).await;

    // The second call must return immediately with an error
    // signalling concurrent-pairing rejection. The underlying
    // `TvManagerError::PairingFailed` carries an inner
    // `tisvlay_core::Error::backend("...", "already pairing")`;
    // the D-Bus surface maps this to a `Failed` error whose message
    // contains "already pairing".
    let err = second.expect_err("second re_pair while first is in-flight must fail");
    // The anyhow::Error chains daemon-side `PairingFailed("already
    // pairing")` through the D-Bus surface and the DaemonClient's
    // contextual wrapper ("Failed to re_pair for TV: <uuid>"). The
    // "already pairing" string lives in the underlying cause, not
    // the outermost message. Format with `{:#}` (alternate) to
    // include the full chain.
    let chain = format!("{:#}", err);
    assert!(
        chain.contains("already pairing"),
        "second re_pair error chain should mention 'already pairing'; got: {chain}",
    );

    // Clean up: cancel the still-hanging first re-pair so its
    // task resolves (with cancelled error) before the test daemon
    // drops. Otherwise the spawned tokio task could outlive the
    // daemon's bus connection.
    let _ = client.cancel_pair(&tv_uuid).await;
    let first_result = first.await.expect("first re_pair task joins");
    // First re-pair should resolve with an error (cancelled), not
    // succeed. Don't assert on the specific message — the cancel
    // semantics ladder through PairingError -> TvManagerError ->
    // DaemonError and message formatting varies. What matters is
    // that the first call eventually terminated rather than hung.
    assert!(
        first_result.is_err(),
        "first re_pair must terminate with an error after cancel; got Ok",
    );
}
