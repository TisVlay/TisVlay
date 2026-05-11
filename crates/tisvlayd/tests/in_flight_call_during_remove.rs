//! Regression: in-flight `Tv1.*` method calls and `Tv1.Remove` against
//! the same TV produce coherent observable outcomes, never panics or
//! hangs. Locks in the per-TV `Mutex<ManagedTv>` drain semantics
//! documented at [`tv_manager::TvManager::remove_tv`].
//!
//! The three documented sub-cases:
//!
//! - **(a)** Call hasn't reached `get_tv` yet → `TvNotFound`.
//! - **(b)** Call holds the per-TV Arc but is awaiting the inner
//!   Mutex → `NotConnected` after removal closes the connection.
//! - **(c)** Call holds the Mutex during work → blocks the remove
//!   until the in-flight write completes; in-flight call returns Ok.
//!
//! Which sub-case fires on any given run depends on which task wins
//! the per-TV Mutex first (non-deterministic ordering between
//! independently spawned tokio tasks). The test asserts the
//! *invariant* that holds across all three cases: remove always
//! succeeds, the TV is gone from `ObjectManager` afterward, and the
//! in-flight call returns one of the three documented observable
//! results (never a hang, panic, or undocumented error).
//!
//! A future regression in the Mutex-held-across-await pattern (e.g.,
//! refactoring `with_connection` to release the Mutex before the
//! SSAP send) would surface as either a hang, a stale-Ok scenario
//! that doesn't match any of the three cases, or a leftover TV in
//! `list_tvs()` after remove.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::TestDaemon;
use tisvlay_test::PrivateBus;

async fn client() -> DaemonClient {
    let bus_conn = PrivateBus::shared().connect().await.expect("client conn");
    DaemonClient::from_connection(bus_conn)
        .await
        .expect("daemon client")
}

#[tokio::test]
async fn in_flight_tv1_call_and_remove_produce_coherent_outcomes() {
    // `_daemon` binding holds the test daemon alive for the test's
    // duration; dropping it would tear down the bus connection mid-test.
    let _daemon = TestDaemon::with_one_mock_tv("drain_test")
        .await
        .expect("test daemon");
    let client = client().await;
    let tv_uuid = tisvlay_harness::daemon::test_tv_uuid("drain_test").to_string();

    // Establish a live connection by powering on; without this, the
    // mock TV's `connection` is `None` and `set_volume` would
    // immediately return `NotConnected` regardless of timing.
    client.power_on(&tv_uuid).await.expect("power_on");
    // Small settle window for the mock's connect path. Real-time
    // sleep here is acceptable per the established pattern in
    // `reload_resolves_system_profile.rs`; we're not asserting on
    // timing, just letting the connect-on-power-on path finish.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fire set_volume and remove concurrently. Independent tokio
    // tasks — ordering between them is non-deterministic, so this
    // exercises one of the three sub-cases on any given run.
    let tv_a = tv_uuid.clone();
    let client_a = client.clone();
    let set_volume_task = tokio::spawn(async move { client_a.set_volume(&tv_a, 50).await });
    let tv_b = tv_uuid.clone();
    let client_b = client.clone();
    let remove_task = tokio::spawn(async move { client_b.remove_tv(&tv_b).await });

    let set_volume_result = set_volume_task.await.expect("set_volume task joins");
    let remove_result = remove_task.await.expect("remove task joins");

    // Invariant 1: remove always succeeds. The drain semantics
    // guarantee remove can complete regardless of in-flight calls —
    // case (c) blocks the remove until the in-flight finishes, then
    // it proceeds. A future regression that lets in-flight calls
    // hold the Mutex indefinitely would surface as a hang here.
    remove_result.expect("Tv1.Remove should succeed regardless of in-flight call ordering");

    // Invariant 2: set_volume's result matches one of the three
    // documented sub-cases. Any other observable outcome is a
    // contract violation.
    match set_volume_result {
        Ok(()) => {
            // Case (c): in-flight write held the Mutex during SSAP
            // work; remove blocked until it completed.
        }
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.to_lowercase().contains("not found")
                    || msg.contains("Not connected")
                    || msg.to_lowercase().contains("not connected"),
                "set_volume during remove returned an undocumented error — \
                 expected `TvNotFound` (case a) or `NotConnected` (case b) \
                 if remove won the Mutex first, or Ok (case c) if set_volume \
                 won. Got: {msg}",
            );
        }
    }

    // Invariant 3: the TV is gone from `ObjectManager` after remove
    // completes. Confirms the D-Bus path was unregistered as part
    // of `Tv1.Remove`'s contract.
    let tvs = client.list_tvs().await.expect("list_tvs");
    assert!(
        !tvs.iter().any(|t| t.uuid == tv_uuid),
        "TV should be absent from ObjectManager after Tv1.Remove succeeds",
    );
}
