//! End-to-end tests for `Tv1.UpdateConfig` + `Tv1.GetConfig`.
//!
//! Exercises the full GUI/CLI edit pathway:
//!
//!   DaemonClient::get_tv_config(uuid) → Tv1.GetConfig (D-Bus)
//!     → TvManager::get_tv_config_json
//!     → walks `config_schema`, strips `Secret`
//!
//!   DaemonClient::update_tv_config(uuid, patch) → Tv1.UpdateConfig
//!     → TvManager::update_tv_config
//!     → schema validation + atomic in-memory + on-disk apply
//!     → reconnect on vendor-field changes
//!
//! The mock backend declares a `config_schema` matching what an LG-style
//! vendor would expose (`address`, `initial_volume`, etc.) so these
//! tests cover the round-trip without needing a live LG TV.

#![cfg(feature = "mock")]

use std::time::Duration;

use tisvlay_core::{DaemonClient, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;

/// Yield + brief sleep so the daemon's state-change forwarder has a
/// chance to apply emitted `OptionChanged` events to the cached
/// fields on the per-TV `Tv1` interface (which is what `list_tvs`
/// reads). Without this, the test would race the forwarder.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn empty_daemon() -> (TestDaemon, DaemonClient) {
    let config = SystemConfig::default();
    let daemon = TestDaemon::with_config(config).await.unwrap();
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .unwrap();
    (daemon, client)
}

async fn pair_one(daemon: &TestDaemon, client: &DaemonClient, address: &str) -> String {
    daemon.handle().simulate_user_accept(address);
    let config_json = format!(r#"{{"address": "{}"}}"#, address);
    client
        .pair_tv("Edit Target", "mock", &config_json, true)
        .await
        .expect("pairing should succeed")
}

#[tokio::test]
async fn get_config_returns_top_level_and_vendor_fields() {
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://edit_target").await;

    let raw = client
        .get_tv_config(&tv_id)
        .await
        .expect("get_config should succeed");
    let payload: serde_json::Value = serde_json::from_str(&raw).expect("payload is valid JSON");
    let obj = payload.as_object().expect("payload is a JSON object");

    // Top-level fields populated from TvConfig.
    assert_eq!(obj.get("name"), Some(&serde_json::json!("Edit Target")));
    assert_eq!(obj.get("managed"), Some(&serde_json::json!(true)));
    assert_eq!(
        obj.get("system_profile"),
        Some(&serde_json::json!("default")),
    );

    // Vendor fields included from `vendor_config`.
    assert_eq!(
        obj.get("address"),
        Some(&serde_json::json!("mock://edit_target")),
    );
}

#[tokio::test]
async fn update_config_applies_top_level_name_change() {
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://name_test").await;

    let patch = serde_json::json!({ "name": "Renamed" }).to_string();
    client
        .update_tv_config(&tv_id, &patch)
        .await
        .expect("update_config should succeed");
    settle().await;

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).unwrap();
    assert_eq!(tv.name, "Renamed");
}

#[tokio::test]
async fn update_config_applies_managed_flag_change() {
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://managed_test").await;

    let patch = serde_json::json!({ "managed": false }).to_string();
    client
        .update_tv_config(&tv_id, &patch)
        .await
        .expect("update_config should succeed");
    settle().await;

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).unwrap();
    assert!(!tv.managed);
}

#[tokio::test]
async fn update_config_round_trips_through_get_config() {
    // Submit a name change, read back via GetConfig — proves the
    // edit form's prefill cycle (read → user edits → write → read)
    // matches what the GUI will rely on.
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://round_trip").await;

    let patch = serde_json::json!({ "name": "Living Room" }).to_string();
    client.update_tv_config(&tv_id, &patch).await.unwrap();

    let raw = client.get_tv_config(&tv_id).await.unwrap();
    let payload: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(payload.get("name"), Some(&serde_json::json!("Living Room")),);
}

#[tokio::test]
async fn update_config_rejects_unknown_field() {
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://unknown_field").await;

    let patch = serde_json::json!({ "not_a_real_field": "bogus" }).to_string();
    let err = client
        .update_tv_config(&tv_id, &patch)
        .await
        .expect_err("unknown field should be rejected");

    let msg = format!("{:#}", err);
    assert!(
        msg.contains("not_a_real_field"),
        "error message should name the rejected field, got {msg:?}"
    );
}

#[tokio::test]
async fn update_config_rejects_unknown_system_profile() {
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://bad_profile").await;

    let patch = serde_json::json!({ "system_profile": "nonexistent" }).to_string();
    let err = client
        .update_tv_config(&tv_id, &patch)
        .await
        .expect_err("unknown system profile should be rejected");

    let msg = format!("{:#}", err);
    assert!(
        msg.to_lowercase().contains("profile"),
        "error message should mention the profile, got {msg:?}"
    );
}

#[tokio::test]
async fn vendor_field_change_refreshes_cached_vendor_config() {
    // Regression: editing a vendor field via `Tv1.UpdateConfig`
    // writes the on-disk config and reconnects the transport,
    // but the per-TV D-Bus property cache (`Tv1.VendorConfig`,
    // `Tv1.VendorInfo`, `Tv1.Power.SupportsWake`) used to stay
    // at the old values until daemon restart. The
    // `TvVendorConfigChanged` event + `refresh_tv_vendor_state`
    // forwarder handler now keep them in sync.
    //
    // The mock's `address` is a `UserInput` field, so it lives
    // in `Tv1.VendorConfig` (not `Tv1.VendorInfo`, which is
    // narrowed to `Diagnostic`-visibility rows).
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://before_edit").await;
    settle().await;

    let initial = client.list_tvs().await.unwrap();
    let initial_descriptors: Vec<String> = initial
        .iter()
        .find(|t| t.uuid == tv_id)
        .unwrap()
        .vendor_config
        .iter()
        .map(|(_, v)| v.clone())
        .collect();
    assert!(
        initial_descriptors
            .iter()
            .any(|v| v == "mock://before_edit"),
        "sanity: vendor_config should carry the original address; got {initial_descriptors:?}"
    );

    let patch = serde_json::json!({ "address": "mock://after_edit" }).to_string();
    client.update_tv_config(&tv_id, &patch).await.unwrap();
    settle().await;

    let updated = client.list_tvs().await.unwrap();
    let updated_descriptors: Vec<String> = updated
        .iter()
        .find(|t| t.uuid == tv_id)
        .unwrap()
        .vendor_config
        .iter()
        .map(|(_, v)| v.clone())
        .collect();
    assert!(
        updated_descriptors.iter().any(|v| v == "mock://after_edit"),
        "vendor_config should reflect the updated address; got {updated_descriptors:?}"
    );
    assert!(
        !updated_descriptors
            .iter()
            .any(|v| v == "mock://before_edit"),
        "vendor_config should no longer carry the stale original address; got {updated_descriptors:?}"
    );
}

#[tokio::test]
async fn update_config_is_atomic_on_validation_failure() {
    // If any field in a multi-field patch fails validation, no
    // changes should land — neither in memory nor on disk. Otherwise
    // a partial apply would silently corrupt the config.
    let (daemon, client) = empty_daemon().await;
    let tv_id = pair_one(&daemon, &client, "mock://atomic_test").await;

    let patch = serde_json::json!({
        "name": "Should Not Stick",
        "system_profile": "nonexistent_profile",
    })
    .to_string();
    let result = client.update_tv_config(&tv_id, &patch).await;
    assert!(
        result.is_err(),
        "validation failure should reject the patch"
    );

    let tvs = client.list_tvs().await.unwrap();
    let tv = tvs.iter().find(|t| t.uuid == tv_id).unwrap();
    assert_eq!(
        tv.name, "Edit Target",
        "name change must not apply when sibling field fails validation"
    );
}
