//! Per-profile `SystemProfile1` typed properties report the resolved
//! value after walking the inheritance chain, expose
//! `LocallySetFields` for the "set vs inheriting" split, and emit
//! `PropertiesChanged` when written. Counterpart to
//! `profile_objects_visible.rs`, which verifies object-path
//! lifecycle.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SessionAction, SystemConfig,
    SystemProfileConfig, system_profile_path,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

/// Build a config with a default profile that has `on_startup =
/// PowerOn` set + a "guest" profile that has only `on_shutdown =
/// ScreenOff` set. The guest profile inherits `on_startup` from
/// default → expected resolved value is `"power_on"`.
fn config_with_default_and_guest(guest_uuid: Uuid) -> SystemConfig {
    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_startup: Some(SessionAction::PowerOn),
            ..Default::default()
        },
    );
    config.system.profiles.insert(
        guest_uuid,
        SystemProfileConfig {
            name: "guest".to_string(),
            on_shutdown: Some(SessionAction::ScreenOff),
            ..Default::default()
        },
    );
    config
}

async fn properties_proxy<'a>(
    daemon: &'a TestDaemon,
    uuid: Uuid,
) -> zbus::fdo::PropertiesProxy<'a> {
    zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy")
}

async fn get_property<T>(proxy: &zbus::fdo::PropertiesProxy<'_>, name: &str) -> T
where
    T: TryFrom<zbus::zvariant::OwnedValue>,
    <T as TryFrom<zbus::zvariant::OwnedValue>>::Error: std::fmt::Debug,
{
    let value = proxy
        .get("org.tisvlay.SystemProfile1".try_into().unwrap(), name)
        .await
        .expect("Properties.Get");
    let owned: zbus::zvariant::OwnedValue = value.try_to_owned().expect("OwnedValue");
    T::try_from(owned).expect("type conversion")
}

#[tokio::test]
async fn typed_properties_report_resolved_values_and_locally_set_fields() {
    let guest_uuid = Uuid::new_v4();
    let config = config_with_default_and_guest(guest_uuid);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    let proxy = properties_proxy(&daemon, guest_uuid).await;

    // Resolved chain: guest has on_shutdown explicit → "screen_off".
    // on_startup is unset on guest, so resolves from default →
    // "power_on".
    let on_shutdown: String = get_property(&proxy, "OnShutdown").await;
    assert_eq!(on_shutdown, "screen_off");
    let on_startup: String = get_property(&proxy, "OnStartup").await;
    assert_eq!(
        on_startup, "power_on",
        "guest inherits on_startup from default, which set it to power_on",
    );

    // LocallySetFields only lists fields the guest profile actually sets.
    let locally_set: Vec<String> = get_property(&proxy, "LocallySetFields").await;
    assert_eq!(
        locally_set,
        vec!["on_shutdown".to_string()],
        "guest body explicitly sets only on_shutdown",
    );
}

#[tokio::test]
async fn properties_set_emits_changed_signal() {
    let guest_uuid = Uuid::new_v4();
    let config = config_with_default_and_guest(guest_uuid);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    let proxy = properties_proxy(&daemon, guest_uuid).await;
    let mut signals = proxy
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged");

    // Tiny pause so the subscriber's match rule is registered before
    // the daemon emits.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Set OnStartup to "screen_on" — different from the inherited
    // "power_on" default, so resolved value should flip.
    proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
            zbus::zvariant::Value::from("screen_on"),
        )
        .await
        .expect("Properties.Set");

    // Read back: explicit value should win.
    let on_startup: String = get_property(&proxy, "OnStartup").await;
    assert_eq!(on_startup, "screen_on");

    // LocallySetFields should now include on_startup.
    let locally_set: Vec<String> = get_property(&proxy, "LocallySetFields").await;
    assert!(
        locally_set.contains(&"on_startup".to_string()),
        "after explicit set, on_startup should be locally-set; got {:?}",
        locally_set,
    );

    // Drain PropertiesChanged signals and confirm we saw an OnStartup
    // notification on SystemProfile1.
    let mut saw_on_startup = false;
    let deadline = tokio::time::sleep(Duration::from_millis(200));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            sig = signals.next() => {
                let Some(sig) = sig else { break };
                let args = match sig.args() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                if args.interface_name.as_str() != "org.tisvlay.SystemProfile1" {
                    continue;
                }
                if args.changed_properties.contains_key("OnStartup") {
                    saw_on_startup = true;
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert!(
        saw_on_startup,
        "PropertiesChanged should have fired for OnStartup",
    );
}

#[tokio::test]
async fn set_rejects_invalid_session_action_value() {
    let guest_uuid = Uuid::new_v4();
    let config = config_with_default_and_guest(guest_uuid);
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    let proxy = properties_proxy(&daemon, guest_uuid).await;
    let err = proxy
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
            zbus::zvariant::Value::from("not_a_real_action"),
        )
        .await
        .expect_err("Properties.Set with invalid SessionAction should fail");

    // The daemon maps SessionAction parse errors to InvalidArgs.
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("invalid"),
        "expected InvalidArgs error, got: {err:?}",
    );
}
