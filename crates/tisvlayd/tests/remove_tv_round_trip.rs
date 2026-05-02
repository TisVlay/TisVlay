//! Regression: `RemoveTv` over D-Bus with a UUID-typed TV ID.
//!
//! After the Phase 8.1 UUID identity migration, every TV's canonical
//! ID is a hyphenated UUID v4 (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
//! D-Bus path components forbid hyphens, so [`tv_path`] sanitizes
//! `-` → `_` on the way to the wire — and the daemon's `RemoveTv`
//! handler must reverse that on the way back, otherwise it looks up
//! the underscored form against an `IndexMap` keyed by the canonical
//! hyphenated UUID and returns `TvNotFound`.
//!
//! Pre-8.1 sanitized-name IDs (`living_room`, `office`) had no
//! hyphens so the round-trip was lossless and the bug was invisible.
//! This test exercises the full GUI/CLI removal flow:
//! `DaemonClient::remove_tv(uuid)` → `tv_path(uuid)` (mangled wire form)
//! → daemon `RemoveTv` handler → daemon's TV map.
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
    let tv_id = client
        .pair_tv("To Remove", "mock", config_json, true)
        .await
        .expect("pairing should succeed");

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
