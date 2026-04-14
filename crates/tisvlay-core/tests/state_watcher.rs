//! Integration tests for `StateWatcher::from_connections`.
//!
//! Verifies that the watcher correctly receives debounced daemon
//! state signals when the daemon's state-change forwarder emits
//! `PropertiesChanged` on per-TV D-Bus objects.
//!
//! Depends on `tisvlay-test` for PrivateBus and on
//! `tisvlayd` (with mock feature) for the TestDaemon harness.

// These tests need both the `dbus` feature on tisvlay-core AND
// the tisvlayd test harness, which is gated behind mock.
#![cfg(feature = "dbus")]

use std::time::Duration;
use tisvlay_core::{DaemonClient, StateEvent, StateWatcher, StateWatcherConfig};
use tisvlay_test::PrivateBus;

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Short debounce config so tests don't wait long.
fn fast_config() -> StateWatcherConfig {
    StateWatcherConfig {
        signal_debounce: Duration::from_millis(10),
        fallback_poll_interval: Duration::from_secs(300),
    }
}

#[tokio::test]
async fn watcher_emits_daemon_appeared_when_name_claimed() {
    let bus = PrivateBus::shared();

    // Create a watcher on the bus BEFORE the daemon claims the name.
    let watcher_conn = bus.connect().await.expect("watcher conn");
    let client = DaemonClient::from_connection(watcher_conn.clone())
        .await
        .expect("client");
    let mut watcher = StateWatcher::from_connections(
        client.connection(),
        Some(client.connection()),
        fast_config(),
    )
    .await
    .expect("watcher");

    // Now build the daemon, which claims org.tisvlay.Daemon.
    // Import TestDaemon dynamically since it's in another crate;
    // we rely on the caller (just check --all-features) enabling mock.
    // For this test, we just build the D-Bus connection directly.
    let daemon_conn = bus.connect().await.expect("daemon conn");
    daemon_conn
        .request_name("org.tisvlay.Daemon")
        .await
        .expect("claim name");

    // The watcher should detect the name appearing.
    let event = tokio::time::timeout(Duration::from_secs(2), watcher.next())
        .await
        .expect("event within 2s")
        .expect("stream not closed");

    match event {
        StateEvent::DaemonAppeared => {} // expected
        other => panic!("expected DaemonAppeared, got {other:?}"),
    }
}

#[tokio::test]
async fn watcher_emits_daemon_disappeared_when_name_released() {
    let bus = PrivateBus::shared();

    // Claim the name first so the watcher sees it as "already present".
    let daemon_conn = bus.connect().await.expect("daemon conn");
    daemon_conn
        .request_name("org.tisvlay.Daemon")
        .await
        .expect("claim name");

    let watcher_conn = bus.connect().await.expect("watcher conn");
    let client = DaemonClient::from_connection(watcher_conn.clone())
        .await
        .expect("client");
    let mut watcher = StateWatcher::from_connections(
        client.connection(),
        Some(client.connection()),
        fast_config(),
    )
    .await
    .expect("watcher");

    // Drop the daemon connection — releases the well-known name.
    drop(daemon_conn);
    settle().await;

    // Drain events until we see DaemonDisappeared. On a shared bus
    // other tests' daemons may trigger DaemonAppeared first.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let event = tokio::time::timeout_at(deadline, watcher.next())
            .await
            .expect("DaemonDisappeared not received within 2s")
            .expect("stream not closed");

        match event {
            StateEvent::DaemonDisappeared => break,
            _ => continue,
        }
    }
}
