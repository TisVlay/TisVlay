//! Regression test for https://github.com/ruffsl/TisVlay/issues/24
//!
//! The daemon should shut down promptly even when connection attempts
//! are in flight. Before the fix, spawned reconnect tasks were
//! fire-and-forget `tokio::spawn` calls with no cancellation — the
//! daemon would block for the full retry timeout (up to ~50s for LG)
//! after receiving SIGINT.
//!
//! The bug has two facets:
//!
//! 1. `close_all_connections()` blocks on the per-TV mutex held by an
//!    in-flight reconnect (which holds the lock across the entire
//!    `connect_tv()` call, including retries and timeouts).
//!
//! 2. Even if `close_all_connections` returned immediately, the
//!    spawned reconnect task itself is never cancelled — it runs to
//!    completion regardless of shutdown.

#![cfg(feature = "mock")]

use std::time::Duration;
use tisvlay_harness::daemon::TestDaemon;

/// Simulates the daemon's shutdown sequence (same as main.rs after
/// SIGINT) and asserts it completes within a bounded time, even when
/// a background reconnect task is holding a per-TV lock inside a slow
/// `connect()`.
#[tokio::test]
async fn shutdown_completes_promptly_with_inflight_reconnect() {
    let daemon = TestDaemon::with_one_mock_tv("slow_tv").await.unwrap();

    // Inject a connect delay long enough that we can observe shutdown
    // blocking if the bug is present.
    daemon.handle().set_connect_delay(Duration::from_secs(10));

    // Simulate what connect_all_on_restart does: spawn a background
    // reconnect that holds the per-TV lock across the slow connect.
    let manager = daemon.manager().clone();
    tokio::spawn(async move {
        let _ = manager.reconnect("slow_tv").await;
    });

    // Let the reconnect task start and enter the connect delay.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Simulate daemon shutdown (same as main.rs after wait_for_signals).
    // If the bug is present, close_all_connections blocks on the per-TV
    // mutex held by the reconnect task for the remaining ~10s of the
    // connect delay.
    let shutdown = async {
        daemon.manager().close_all_connections().await;
        drop(daemon);
    };

    let result = tokio::time::timeout(Duration::from_secs(2), shutdown).await;
    assert!(
        result.is_ok(),
        "daemon shutdown blocked by in-flight reconnect task \
         (close_all_connections stuck on per-TV mutex held across slow connect)"
    );
}
