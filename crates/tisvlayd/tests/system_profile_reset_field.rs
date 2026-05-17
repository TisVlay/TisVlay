//! `SystemProfile1.ResetField` clears one field back to inheriting.
//! `PropertiesChanged` fires for the affected typed property and for
//! `LocallySetFields`.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SessionAction, SystemConfig,
    SystemProfileConfig, system_profile_path,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;
use zbus::Proxy;

/// Minimal proxy that exposes the methods on `SystemProfile1` (which
/// `zbus::fdo::PropertiesProxy` doesn't carry, only its standard
/// `Get`/`Set`).
async fn profile_proxy<'a>(daemon: &'a TestDaemon, uuid: Uuid) -> Proxy<'a> {
    Proxy::new(
        daemon.connection(),
        DAEMON_SERVICE,
        system_profile_path(&uuid),
        "org.tisvlay.SystemProfile1",
    )
    .await
    .expect("Proxy")
}

#[tokio::test]
async fn reset_field_restores_inheritance_and_fires_changed_signals() {
    let guest_uuid = Uuid::new_v4();
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
            // Guest explicitly overrides on_startup to a different value.
            on_startup: Some(SessionAction::ScreenOn),
            ..Default::default()
        },
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    let props = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&guest_uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    // Sanity: guest reports its explicit value.
    let pre: String = props
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string");
    assert_eq!(pre, "screen_on");
    let pre_set: Vec<String> = props
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string array");
    assert!(pre_set.contains(&"on_startup".to_string()));

    // Subscribe to PropertiesChanged before issuing the reset so the
    // emit is captured.
    let mut signals = props
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Call ResetField("on_startup") on the SystemProfile1 interface.
    let profile = profile_proxy(&daemon, guest_uuid).await;
    profile
        .call_method("ResetField", &("on_startup"))
        .await
        .expect("ResetField");

    // After reset: resolved value falls back to the default's
    // on_startup ("power_on") and LocallySetFields no longer lists
    // on_startup.
    let post: String = props
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string");
    assert_eq!(
        post, "power_on",
        "after ResetField, on_startup should fall through to default",
    );
    let post_set: Vec<String> = props
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "LocallySetFields",
        )
        .await
        .expect("Get")
        .try_into()
        .expect("string array");
    assert!(
        !post_set.contains(&"on_startup".to_string()),
        "after ResetField, on_startup should be inheriting (not in LocallySetFields); got {:?}",
        post_set,
    );

    // PropertiesChanged should have fired for both OnStartup and
    // LocallySetFields.
    let mut saw_on_startup = false;
    let mut saw_locally_set = false;
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
                }
                if args.changed_properties.contains_key("LocallySetFields") {
                    saw_locally_set = true;
                }
                if saw_on_startup && saw_locally_set {
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert!(saw_on_startup, "PropertiesChanged for OnStartup expected");
    assert!(
        saw_locally_set,
        "PropertiesChanged for LocallySetFields expected",
    );
}
