//! `UserProfileBody` uses `deny_unknown_fields` at the dict-derive
//! boundary so an `a{sv}` arriving with an unrecognized key (typo,
//! forward-compat slack from a newer client, etc.) is rejected by the
//! deserializer rather than silently absorbed.

use std::collections::HashMap;
use std::path::PathBuf;

use tisvlay_core::{SessionClient, UserConfig};
use tisvlay_harness::session::TestSession;
use zbus::zvariant::Value;

#[tokio::test]
async fn unknown_field_in_body_dict_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path: PathBuf = tmp.path().join("session.toml");
    UserConfig::default().save_to(&config_path).unwrap();

    let _session = TestSession::with_user_config_path("unknown_key_tv", config_path)
        .await
        .expect("test session");

    let bus = tisvlay_test::PrivateBus::shared();
    let conn = bus.connect().await.unwrap();
    let client = SessionClient::from_connection(conn.clone()).await.unwrap();

    // Create a profile so the UUID is real on the wire.
    let uuid = client.create_user_profile("typotest").await.unwrap();

    // Build a body dict carrying a misspelled key (`on_session_lockk`).
    // `SerializeDict` would emit this key on the wire, and the session's
    // `DeserializeDict` with `deny_unknown_fields` should refuse to parse it.
    let mut bad_body: HashMap<&str, Value<'_>> = HashMap::new();
    bad_body.insert("name", Value::from("typotest"));
    bad_body.insert("on_session_lockk", Value::from("screen_off")); // typo

    let proxy = zbus::Proxy::new(
        &conn,
        "org.tisvlay.Session",
        "/org/tisvlay/Session",
        "org.tisvlay.Session1",
    )
    .await
    .expect("proxy");

    let result = proxy
        .call_method("UpdateUserProfile", &(uuid.to_string(), bad_body))
        .await;

    let err = result.expect_err("unknown key must be rejected");
    let chain = err.to_string().to_lowercase();
    assert!(
        chain.contains("unknown") || chain.contains("invalidargs") || chain.contains("invalid"),
        "expected deserialize-boundary rejection on unknown key; got: {err:?}",
    );
}
