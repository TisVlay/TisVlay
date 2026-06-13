//! `Manager1.GetEffectiveSystemProfile(tv_id)` returns the resolved
//! body the daemon will dispatch against: per-TV assignment body
//! merged with default body merged with hardcoded baseline. Every
//! Option<T> field in the returned body must be Some — the wire body
//! is fully resolved at this slot.

#![cfg(feature = "mock")]

use std::collections::HashMap;
use tisvlay_core::{
    AutomateWhen, DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SessionAction, SystemConfig,
    SystemProfileConfig, TvConfig,
};
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use uuid::Uuid;

fn mock_tv_with_profile(tv_id: &str, profile_uuid: Uuid) -> TvConfig {
    let mut tv = TvConfig::with_uuid(test_tv_uuid(tv_id), tv_id, "mock", true);
    let mut vendor_table = HashMap::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String(format!("mock://{}", tv_id)),
    );
    tv.vendor_config.insert(
        "mock".to_string(),
        toml::Value::Table(vendor_table.into_iter().collect()),
    );
    tv.profile.system = profile_uuid;
    tv
}

#[tokio::test]
async fn effective_falls_through_to_default_then_baseline() {
    // TV is assigned to a non-default profile that has *only* `on_startup`
    // set. `on_shutdown` should fall through to default; default has its
    // own value courtesy `ensure_default`. `on_reboot` (which the default
    // also sets via full_default) falls through to the baseline too.
    let custom_uuid = Uuid::new_v4();
    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        custom_uuid,
        SystemProfileConfig {
            name: "custom".to_string(),
            on_startup: Some(SessionAction::ScreenOn),
            ..Default::default()
        },
    );
    let tv_id = "inherit_tv";
    config.tv.push(mock_tv_with_profile(tv_id, custom_uuid));

    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let tv_uuid = test_tv_uuid(tv_id).to_string();
    let effective = client
        .get_effective_system_profile(&tv_uuid)
        .await
        .expect("effective fetch");

    // Every field is Some after resolution.
    assert!(effective.on_startup.is_some());
    assert!(effective.on_shutdown.is_some());
    assert!(effective.on_reboot.is_some());
    assert!(effective.on_no_session.is_some());
    assert!(effective.on_sleep.is_some());
    assert!(effective.on_wake.is_some());
    assert!(effective.on_dpms_off.is_some());
    assert!(effective.on_dpms_on.is_some());
    assert!(effective.automate_when.is_some());

    // Local override wins.
    assert_eq!(effective.on_startup, Some(SessionAction::ScreenOn));

    // Default (via ensure_default → full_default) values fill in the rest.
    let defaults = client
        .get_resolved_system_profile_defaults()
        .await
        .expect("defaults");
    assert_eq!(effective.on_shutdown, defaults.on_shutdown);
    assert_eq!(effective.on_reboot, defaults.on_reboot);
    assert_eq!(effective.automate_when, defaults.automate_when);

    // The displayed name is the assigned profile's name, not "default".
    assert_eq!(effective.name, "custom");
}

#[tokio::test]
async fn effective_for_default_assigned_tv_matches_default_body() {
    // TV assigned to the reserved-default UUID should resolve to the
    // default profile body (which `ensure_default` populated with
    // `full_default`). Every Option<T> field on the effective body
    // matches what `GetResolvedSystemProfileDefaults` returns.
    let mut config = SystemConfig::default();
    let tv_id = "default_tv";
    config
        .tv
        .push(mock_tv_with_profile(tv_id, DEFAULT_SYSTEM_PROFILE_UUID));

    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let tv_uuid = test_tv_uuid(tv_id).to_string();
    let effective = client
        .get_effective_system_profile(&tv_uuid)
        .await
        .expect("effective fetch");

    let defaults = client
        .get_resolved_system_profile_defaults()
        .await
        .expect("defaults");
    assert_eq!(effective.on_startup, defaults.on_startup);
    assert_eq!(effective.on_shutdown, defaults.on_shutdown);
    assert_eq!(effective.automate_when, defaults.automate_when);
}

#[tokio::test]
async fn resolved_defaults_baseline_is_stable() {
    // The hardcoded baseline body should be the same regardless of
    // catalog state — clients can call this at startup once.
    let (_daemon, client) = {
        let daemon = TestDaemon::with_config(SystemConfig::default())
            .await
            .expect("daemon");
        let client = DaemonClient::from_connection(daemon.connection().clone())
            .await
            .expect("client");
        (daemon, client)
    };

    let defaults = client.get_resolved_system_profile_defaults().await.unwrap();
    // Baseline: every Some.
    assert_eq!(defaults.on_startup, Some(SessionAction::PowerOn));
    assert_eq!(defaults.on_shutdown, Some(SessionAction::PowerOff));
    assert_eq!(defaults.on_wake, Some(SessionAction::PowerOn));
    // DPMS actions mirror the host display: blank -> ScreenOff, wake -> ScreenOn.
    assert_eq!(defaults.on_dpms_off, Some(SessionAction::ScreenOff));
    assert_eq!(defaults.on_dpms_on, Some(SessionAction::ScreenOn));
    assert_eq!(defaults.automate_when, Some(AutomateWhen::Always));
    assert_eq!(defaults.name, "default");
}
