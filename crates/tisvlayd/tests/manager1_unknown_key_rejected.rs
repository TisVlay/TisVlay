//! `SystemProfileBody` uses `deny_unknown_fields` at the dict-derive
//! boundary so an `a{sv}` arriving with an unrecognized key (typo,
//! forward-compat slack from a newer client, etc.) is rejected by the
//! deserializer rather than silently absorbed. The wire surface fails
//! the call instead of accepting a partially-typed body.

#![cfg(feature = "mock")]

use std::collections::HashMap;
use tisvlay_core::{DaemonClient, SystemConfig};
use tisvlay_harness::daemon::TestDaemon;
use zbus::zvariant::Value;

#[tokio::test]
async fn unknown_field_in_body_dict_is_rejected() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let uuid = client.create_system_profile("typotest").await.unwrap();

    // Build a body dict carrying a misspelled key (`on_startupp`).
    // `SerializeDict` would emit this key on the wire, and the
    // daemon's `DeserializeDict` with `deny_unknown_fields` should
    // refuse to parse it.
    let mut bad_body: HashMap<&str, Value<'_>> = HashMap::new();
    bad_body.insert("name", Value::from("typotest"));
    bad_body.insert("on_startupp", Value::from("power_on")); // typo

    let conn = client.connection().clone();
    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Daemon",
        "/org/tisvlay/Manager",
        "org.tisvlay.Manager1",
    )
    .await
    .expect("proxy");

    let result = proxy
        .call_method("UpdateSystemProfile", &(uuid.to_string(), bad_body))
        .await;

    let err = result.expect_err("unknown key must be rejected");
    let chain = err.to_string().to_lowercase();
    assert!(
        // Any rejection name is fine — the contract is "this call fails",
        // not "fails with a specific named error". InvalidArgs is the
        // canonical zbus parse-failure surface, but the daemon may also
        // surface its own typed error.
        chain.contains("unknown") || chain.contains("invalidargs") || chain.contains("invalid"),
        "expected deserialize-boundary rejection on unknown key; got: {err:?}",
    );
}
