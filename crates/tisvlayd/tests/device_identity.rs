//! Live device-identity flow: a backend's reported identity (CEC-style)
//! surfaces in `Tv1.VendorInfo` and `Tv1.Brand`, and refreshes when the
//! backend re-queries on reconnect — e.g. a CEC dongle moved to a different
//! TV. Driven through the mock's `set_device_identity` handle.

#![cfg(feature = "mock")]

use std::time::Duration;

use tisvlay_core::{DaemonClient, DeviceIdentity};
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

fn identity(osd: &str, raw: &str, name: &str, canonical: &str, version: &str) -> DeviceIdentity {
    DeviceIdentity {
        osd_name: Some(osd.to_string()),
        vendor_raw: Some(raw.to_string()),
        vendor_name: Some(name.to_string()),
        vendor_canonical: Some(canonical.to_string()),
        cec_version: Some(version.to_string()),
    }
}

#[tokio::test]
async fn identity_surfaces_in_vendor_info_and_brand_and_refreshes_on_reconnect() {
    let id = "ident";
    let tv_uuid = test_tv_uuid(id).to_string();
    let addr = "mock://ident";
    let daemon = TestDaemon::with_one_mock_tv(id).await.expect("daemon");
    let client = client().await;

    // Seed an LG identity, then connect so the initial snapshot carries it.
    daemon
        .handle()
        .set_device_identity(addr, identity("OLED42C3PU", "00-e0-91", "LG", "lg", "1.4"));
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.vendor_info
            .iter()
            .any(|(l, v)| l == "Device name" && v == "OLED42C3PU"),
        "OSD name row present: {:?}",
        tv.vendor_info,
    );
    assert!(
        tv.vendor_info
            .iter()
            .any(|(l, v)| l == "Vendor" && v == "LG (00-e0-91)"),
        "vendor row shows name + raw: {:?}",
        tv.vendor_info,
    );
    assert_eq!(
        tv.brand, "lg",
        "brand resolves to the device vendor, not 'mock'"
    );

    // Model the dongle re-plugged into a Samsung set: change the identity and
    // reconnect. VendorInfo and Brand must follow the new TV.
    daemon.handle().set_device_identity(
        addr,
        identity("QN90A", "00-00-f0", "Samsung", "samsung", "2.0"),
    );
    daemon
        .manager()
        .reconnect(&tv_uuid)
        .await
        .expect("reconnect");
    settle().await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    let tv = tvs.iter().find(|t| t.uuid == tv_uuid).expect("tv present");
    assert!(
        tv.vendor_info
            .iter()
            .any(|(l, v)| l == "Vendor" && v == "Samsung (00-00-f0)"),
        "identity refreshed on reconnect: {:?}",
        tv.vendor_info,
    );
    assert_eq!(tv.brand, "samsung", "brand refreshed on reconnect");
}
