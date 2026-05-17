//! `Tv1.SystemProfile` accepts profile UUIDs only; unknown UUIDs are
//! rejected with `InvalidArgs`, malformed UUID strings are rejected
//! with `InvalidArgs`, and known UUIDs route through to a normal
//! property write.
//!
//! Guards the v2 wire contract — the setter is the single supported
//! path for assigning a TV to a profile, and clients route on the
//! standard `org.freedesktop.DBus.Error.InvalidArgs` name.

#![cfg(feature = "mock")]

use tisvlay_core::{DAEMON_SERVICE, DEFAULT_SYSTEM_PROFILE_UUID, SystemConfig, tv_path};
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use uuid::Uuid;

async fn props_proxy<'a>(daemon: &'a TestDaemon, tv_id: &str) -> zbus::fdo::PropertiesProxy<'a> {
    zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(tv_path(tv_id))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy")
}

fn config_with_one_tv() -> SystemConfig {
    let mut config = SystemConfig::default();
    let mut tv = tisvlay_core::TvConfig::with_uuid(
        test_tv_uuid("uuid_setter_tv"),
        "uuid_setter_tv",
        "mock",
        true,
    );
    let mut vendor_table = std::collections::HashMap::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String("mock://uuid_setter_tv".to_string()),
    );
    tv.vendor_config.insert(
        "mock".to_string(),
        toml::Value::Table(vendor_table.into_iter().collect()),
    );
    config.tv.push(tv);
    config
}

#[tokio::test]
async fn set_rejects_unknown_uuid_with_invalid_args() {
    let daemon = TestDaemon::with_config(config_with_one_tv())
        .await
        .expect("daemon");
    let tv_id = test_tv_uuid("uuid_setter_tv").to_string();

    let proxy = props_proxy(&daemon, &tv_id).await;
    // Well-formed UUID that doesn't exist in the catalog.
    let absent = Uuid::new_v4().to_string();
    let err = proxy
        .set(
            "org.tisvlay.Tv1".try_into().unwrap(),
            "SystemProfile",
            zbus::zvariant::Value::from(absent.as_str()),
        )
        .await
        .expect_err("setting an unknown profile UUID should fail");
    let name = err.to_string().to_lowercase();
    assert!(
        name.contains("invalidargs") || name.contains("unknown"),
        "error should surface as InvalidArgs / Unknown system profile; got {err:?}",
    );
}

#[tokio::test]
async fn set_rejects_malformed_uuid_with_invalid_args() {
    let daemon = TestDaemon::with_config(config_with_one_tv())
        .await
        .expect("daemon");
    let tv_id = test_tv_uuid("uuid_setter_tv").to_string();

    let proxy = props_proxy(&daemon, &tv_id).await;
    let err = proxy
        .set(
            "org.tisvlay.Tv1".try_into().unwrap(),
            "SystemProfile",
            zbus::zvariant::Value::from("not-a-uuid"),
        )
        .await
        .expect_err("setting a malformed UUID should fail");
    let name = err.to_string().to_lowercase();
    assert!(
        name.contains("invalidargs") || name.contains("invalid"),
        "error should surface as InvalidArgs / Invalid UUID; got {err:?}",
    );
}

#[tokio::test]
async fn set_accepts_default_uuid() {
    let daemon = TestDaemon::with_config(config_with_one_tv())
        .await
        .expect("daemon");
    let tv_id = test_tv_uuid("uuid_setter_tv").to_string();

    let proxy = props_proxy(&daemon, &tv_id).await;
    // The default UUID is always present (ensure_default at load).
    proxy
        .set(
            "org.tisvlay.Tv1".try_into().unwrap(),
            "SystemProfile",
            zbus::zvariant::Value::from(DEFAULT_SYSTEM_PROFILE_UUID.to_string()),
        )
        .await
        .expect("Set Tv1.SystemProfile to default UUID");

    let new_value: String = proxy
        .get("org.tisvlay.Tv1".try_into().unwrap(), "SystemProfile")
        .await
        .expect("Get Tv1.SystemProfile")
        .try_into()
        .expect("string");
    assert_eq!(new_value, DEFAULT_SYSTEM_PROFILE_UUID.to_string());
}
