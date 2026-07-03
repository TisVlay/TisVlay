//! A backend's device identity (CEC-style: OSD name → model, vendor →
//! manufacturer) flows into `Tv1.HardwareId` (the GUI's Make/Model rows) and
//! refreshes when the backend re-queries on reconnect — e.g. a CEC dongle
//! moved to a different TV. Driven through the mock's `set_hardware_id`.

#![cfg(feature = "mock")]

use std::time::Duration;

use tisvlay_core::{DaemonClient, HardwareId};
use tisvlay_harness::daemon::{TestDaemon, test_tv_uuid};
use tisvlay_test::PrivateBus;

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn client() -> DaemonClient {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    DaemonClient::from_connection(conn)
        .await
        .expect("daemon client")
}

fn hw(manufacturer: &str, model: &str) -> HardwareId {
    HardwareId {
        manufacturer: Some(manufacturer.to_string()),
        model: Some(model.to_string()),
        serial: None,
    }
}

#[tokio::test]
async fn hardware_id_refreshes_on_reconnect_to_a_different_device() {
    let id = "hwid";
    let tv_uuid = test_tv_uuid(id).to_string();
    let addr = "mock://hwid";
    let daemon = TestDaemon::with_one_mock_tv(id).await.expect("daemon");
    let client = client().await;

    // Seed an LG identity, connect.
    daemon
        .handle()
        .set_hardware_id(addr, hw("LG", "OLED42C3PU"));
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert_eq!(tv.hardware_id.manufacturer.as_deref(), Some("LG"));
    assert_eq!(tv.hardware_id.model.as_deref(), Some("OLED42C3PU"));

    // Model a dongle re-plugged into a Samsung set: change the identity and
    // reconnect. HardwareId must follow the new TV.
    daemon
        .handle()
        .set_hardware_id(addr, hw("Samsung", "QN90A"));
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert_eq!(
        tv.hardware_id.manufacturer.as_deref(),
        Some("Samsung"),
        "Make refreshed on reconnect",
    );
    assert_eq!(tv.hardware_id.model.as_deref(), Some("QN90A"));
}
