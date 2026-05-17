//! `SystemProfile1.ReferencedBy` lists the UUIDs of TVs whose
//! `profile.system` resolves to this profile. Reassigning a TV's
//! profile shifts ReferencedBy on both the previously-affected and
//! newly-affected profiles via the `SystemProfileBodyChanged`
//! fan-out from the daemon.

#![cfg(feature = "mock")]

use std::collections::HashSet;
use std::time::Duration;

use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, SystemConfig,
    SystemProfileConfig, system_profile_path,
};
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use uuid::Uuid;

async fn referenced_by(daemon: &TestDaemon, uuid: Uuid) -> HashSet<String> {
    let proxy = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(system_profile_path(&uuid))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");
    let raw: zbus::zvariant::OwnedValue = proxy
        .get(
            "org.tisvlay.SystemProfile1".try_into().unwrap(),
            "ReferencedBy",
        )
        .await
        .expect("Get ReferencedBy")
        .try_to_owned()
        .expect("OwnedValue");
    let list: Vec<String> = raw.try_into().expect("string array");
    list.into_iter().collect()
}

#[tokio::test]
async fn referenced_by_tracks_tv_profile_assignment_changes() {
    let tv_id = test_tv_uuid("ref_tv").to_string();
    let guest_uuid = Uuid::new_v4();

    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        DEFAULT_SYSTEM_PROFILE_UUID,
        SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..Default::default()
        },
    );
    config.system.profiles.insert(
        guest_uuid,
        SystemProfileConfig {
            name: "guest".to_string(),
            ..Default::default()
        },
    );
    // Build a TV that explicitly points at the guest profile by UUID.
    let mut tv = tisvlay_core::TvConfig::with_uuid(test_tv_uuid("ref_tv"), "ref_tv", "mock", true);
    tv.profile.system = guest_uuid;
    let mut vendor_table = std::collections::HashMap::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String(format!("mock://{}", "ref_tv")),
    );
    tv.vendor_config.insert(
        "mock".to_string(),
        toml::Value::Table(vendor_table.into_iter().collect()),
    );
    config.tv.push(tv);

    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    // Tiny pause so the initial body-changed events (if any) settle.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let guest_refs = referenced_by(&daemon, guest_uuid).await;
    assert!(
        guest_refs.contains(&tv_id),
        "guest.ReferencedBy must include {tv_id} initially; got {guest_refs:?}",
    );

    let default_refs = referenced_by(&daemon, DEFAULT_SYSTEM_PROFILE_UUID).await;
    assert!(
        !default_refs.contains(&tv_id),
        "default.ReferencedBy must not include {tv_id} initially; got {default_refs:?}",
    );

    // Reassign the TV to "default" via the Tv1.SystemProfile setter.
    let tv_props = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(format!("/org/tisvlay/Tv/{}", tv_id.replace('-', "_")))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy on Tv");
    tv_props
        .set(
            "org.tisvlay.Tv1".try_into().unwrap(),
            "SystemProfile",
            zbus::zvariant::Value::from(DEFAULT_SYSTEM_PROFILE_UUID.to_string()),
        )
        .await
        .expect("Set Tv1.SystemProfile");

    // Give the state-forwarder a tick to refresh both profile caches.
    tokio::time::sleep(Duration::from_millis(80)).await;

    let guest_refs_after = referenced_by(&daemon, guest_uuid).await;
    assert!(
        !guest_refs_after.contains(&tv_id),
        "guest.ReferencedBy must not include {tv_id} after reassign; got {guest_refs_after:?}",
    );

    let default_refs_after = referenced_by(&daemon, DEFAULT_SYSTEM_PROFILE_UUID).await;
    assert!(
        default_refs_after.contains(&tv_id),
        "default.ReferencedBy must include {tv_id} after reassign; got {default_refs_after:?}",
    );
}
