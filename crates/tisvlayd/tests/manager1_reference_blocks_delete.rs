//! `Manager1.DeleteSystemProfile` refuses to delete a profile while at
//! least one TV still references it. Reassign the TV elsewhere first
//! and the same delete succeeds.

#![cfg(feature = "mock")]

use std::collections::HashMap;
use tisvlay_core::{
    DAEMON_SERVICE, DEFAULT_SYSTEM_PROFILE_UUID, DaemonClient, SystemConfig, SystemProfileConfig,
    TvConfig, tv_path,
};
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
async fn delete_blocked_when_tv_references_profile() {
    let referenced = Uuid::new_v4();
    let mut config = SystemConfig::default();
    config.system.profiles.insert(
        referenced,
        SystemProfileConfig {
            name: "referenced".to_string(),
            ..Default::default()
        },
    );
    let tv_id = "ref_tv";
    config.tv.push(build_tv(tv_id, referenced));

    let daemon = TestDaemon::with_config(config).await.expect("daemon");
    let client = DaemonClient::from_connection(daemon.connection().clone())
        .await
        .expect("client");

    // Delete should fail with ProfileReferenced.
    let err = client
        .delete_system_profile(referenced)
        .await
        .expect_err("delete with refs should fail");
    let chain = format!("{err:#}").to_lowercase();
    assert!(
        chain.contains("profilereferenced") || chain.contains("still referenced"),
        "expected ProfileReferenced; got: {chain}",
    );

    // Profile still in the catalog.
    let bodies = client.list_system_profile_bodies().await.unwrap();
    assert!(bodies.contains_key(&referenced), "profile must still exist");

    // Reassign the TV to the default profile via the per-TV property
    // setter (the canonical client-side route for assignment changes).
    let tv_uuid_str = test_tv_uuid(tv_id).to_string();
    let proxy = zbus::fdo::PropertiesProxy::builder(daemon.connection())
        .destination(DAEMON_SERVICE)
        .expect("destination")
        .path(tv_path(&tv_uuid_str))
        .expect("path")
        .build()
        .await
        .expect("PropertiesProxy");
    proxy
        .set(
            "org.tisvlay.Tv1".try_into().unwrap(),
            "SystemProfile",
            zbus::zvariant::Value::from(DEFAULT_SYSTEM_PROFILE_UUID.to_string()),
        )
        .await
        .expect("re-assign TV to default profile");

    // Now delete succeeds.
    client
        .delete_system_profile(referenced)
        .await
        .expect("delete succeeds after re-assignment");

    let bodies_after = client.list_system_profile_bodies().await.unwrap();
    assert!(
        !bodies_after.contains_key(&referenced),
        "profile must be gone after delete; got {bodies_after:?}",
    );
}
