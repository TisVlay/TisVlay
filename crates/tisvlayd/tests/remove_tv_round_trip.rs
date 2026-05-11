//! Regression: removing a UUID-keyed TV through the GUI/CLI surface
//! must not silently miss the daemon's in-memory TV map.
//!
//! Every TV's canonical ID is a hyphenated UUID v4
//! (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
//! D-Bus path components forbid hyphens, so an early implementation
//! that took the path on `Manager1.RemoveTv(o)` and stripped the
//! prefix — without reversing the `-` → `_` sanitization that
//! [`tv_path`] applies — looked up the underscored form against an
//! `IndexMap` keyed by the hyphenated UUID and returned `TvNotFound`.
//! Earlier sanitized-name IDs had no hyphens so the round-trip was
//! lossless and the bug was invisible.
//!
//! The fix routed removal through a per-object `Tv1.Remove()` method,
//! which gives the daemon's handler `&self.tv_id` directly. This
//! test guards the public client surface so the regression can't
//! sneak back regardless of which D-Bus shape `DaemonClient::remove_tv`
//! ends up using.
//!
//! [`tv_path`]: tisvlay_core::tv_path

#![cfg(feature = "mock")]

use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::TestDaemon;

async fn empty_daemon() -> (TestDaemon, DaemonClient) {
    let config = tisvlay_core::SystemConfig::default();
    let daemon = TestDaemon::with_config(config).await.unwrap();
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .unwrap();
    (daemon, client)
}

#[tokio::test]
async fn remove_tv_succeeds_for_uuid_keyed_tv() {
    let (daemon, client) = empty_daemon().await;
    daemon.handle().simulate_user_accept("mock://to_remove");

    let config_json = r#"{"address": "mock://to_remove"}"#;
    // Subscribe before pairing so we can drain to the terminal
    // phase; AddTv is async-spawn, so the TV isn't registered via
    // ObjectManager until `complete` arrives.
    let stream = client.pairing_progress_stream().await.unwrap();
    let tv_id = client
        .pair_tv("To Remove", "mock", config_json, true)
        .await
        .expect("AddTv RPC should succeed");
    let terminal = tisvlay_core::DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_, _| {})
        .await
        .expect("drain should reach terminal");
    assert!(
        matches!(terminal, tisvlay_core::PairTerminal::Complete { .. }),
        "expected Complete, got {terminal:?}"
    );

    // Sanity: the TV is present and identified by a hyphenated UUID.
    assert!(
        uuid::Uuid::parse_str(&tv_id).is_ok(),
        "AddTv should return a hyphenated UUID, got {tv_id:?}"
    );
    assert_eq!(client.list_tvs().await.unwrap().len(), 1);

    // The actual regression: removal via D-Bus must round-trip the
    // path-component sanitization (hyphen ↔ underscore) so the
    // daemon's IndexMap lookup hits.
    client
        .remove_tv(&tv_id)
        .await
        .expect("remove_tv should succeed for a paired UUID-keyed TV");

    assert!(
        client.list_tvs().await.unwrap().is_empty(),
        "TV should be gone from list_tvs after RemoveTv",
    );
}
