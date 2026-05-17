//! Editing a field on the default system profile fans out to every
//! other profile's resolved chain. Guest profile that inherits
//! OnStartup from default must see `PropertiesChanged(OnStartup)`
//! when the default's OnStartup changes.

#![cfg(feature = "mock")]

use std::time::Duration;

use futures_util::stream::StreamExt;
use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SessionAction, SystemConfig,
    SystemProfileConfig, system_profile_path,
};
use tisvlay_harness::daemon::TestDaemon;
use uuid::Uuid;

#[tokio::test]
async fn editing_default_fans_out_to_guest_resolved_values() {
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
            // Guest inherits on_startup → resolved value comes from
            // default ("power_on" initially).
            ..Default::default()
        },
    );
    let daemon = TestDaemon::with_config(config).await.expect("daemon");

    let guest_props = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&guest_uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    // Subscribe to guest's PropertiesChanged BEFORE editing the default.
    let mut signals = guest_props
        .receive_properties_changed()
        .await
        .expect("subscribe PropertiesChanged on guest");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Edit the default profile's OnStartup. Guest's resolved value
    // should shift, and its PropertiesChanged should fire.
    let default_props = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&DEFAULT_SYSTEM_PROFILE_UUID))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");

    default_props
        .set(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
            zbus::zvariant::Value::from("screen_off"),
        )
        .await
        .expect("Set default OnStartup");

    // Drain guest signals; expect to see an OnStartup change with
    // the new resolved value.
    let mut new_value: Option<String> = None;
    let deadline = tokio::time::sleep(Duration::from_millis(300));
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
                if let Some(v) = args.changed_properties.get("OnStartup") {
                    new_value = Some(
                        String::try_from(v).expect("OnStartup is a string"),
                    );
                    break;
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert_eq!(
        new_value,
        Some("screen_off".to_string()),
        "editing default OnStartup must fan out PropertiesChanged on guest's OnStartup",
    );

    // Confirm guest's cached OnStartup property now reads the new
    // resolved value too.
    let post: String = guest_props
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "OnStartup",
        )
        .await
        .expect("Get OnStartup")
        .try_into()
        .expect("string");
    assert_eq!(post, "screen_off");
}
