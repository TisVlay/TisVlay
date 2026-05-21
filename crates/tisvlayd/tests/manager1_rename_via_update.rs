//! Renames fold into `UpdateSystemProfile(uuid, body)` — there is no
//! separate rename RPC. The UUID stays stable across the rename so
//! existing TV references continue to resolve to the renamed body.

#![cfg(feature = "mock")]

use std::collections::HashMap;
use tisvlay_core::{DaemonClient, SystemConfig, TvConfig};
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use uuid::Uuid;

fn build_tv(tv_id: &str, profile_uuid: Uuid) -> TvConfig {
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
async fn rename_via_update_keeps_uuid_stable() {
    let daemon = TestDaemon::with_config(SystemConfig::default())
        .await
        .expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let uuid = client.create_system_profile("before").await.unwrap();
    let body_before = client.get_system_profile(uuid).await.unwrap();
    assert_eq!(body_before.name, "before");

    // Rename via update.
    let mut renamed = body_before.clone();
    renamed.name = "after".to_string();
    client
        .update_system_profile(uuid, &renamed)
        .await
        .expect("rename");

    // Same UUID, new name.
    let after = client
        .get_system_profile(uuid)
        .await
        .expect("body fetched by same UUID");
    assert_eq!(after.name, "after");

    // List confirms the UUID stayed stable and the name updated.
    let bodies = client.list_system_profile_bodies().await.unwrap();
    let entry = bodies.get(&uuid).expect("UUID still in catalog");
    assert_eq!(entry.name, "after");
}

#[tokio::test]
async fn rename_does_not_break_tv_reference() {
    // A TV referencing a profile by UUID continues to resolve to the
    // same body after the profile's name changes. References are by
    // UUID, not by name, so renames are transparent to references.
    let profile_uuid = Uuid::new_v4();
    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        profile_uuid,
        tisvlay_core::SystemProfileConfig {
            name: "initial".to_string(),
            ..Default::default()
        },
    );
    let tv_id = "rename_tv";
    config.tv.push(build_tv(tv_id, profile_uuid));

    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    let tv_uuid = test_tv_uuid(tv_id).to_string();
    let before = client
        .get_effective_system_profile(&tv_uuid)
        .await
        .expect("effective before");
    assert_eq!(before.name, "initial");

    let mut renamed = client.get_system_profile(profile_uuid).await.unwrap();
    renamed.name = "renamed".to_string();
    client
        .update_system_profile(profile_uuid, &renamed)
        .await
        .expect("rename");

    let after = client
        .get_effective_system_profile(&tv_uuid)
        .await
        .expect("effective after");
    assert_eq!(after.name, "renamed", "TV's effective body reflects rename");
}
