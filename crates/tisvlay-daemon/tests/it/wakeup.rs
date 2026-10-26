//! The wake list: the daemon writes the device nodes of the TVs whose
//! system profile lets them wake the PC beside its socket, where the
//! wakeup unit reads them before each sleep.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tisvlay_mock::new_test_parts;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{manager, profile, tv};
use tisvlay_protocol::{bootstrap, keys, rpc};

/// Wait until `path` reads `want`.
async fn wait_contents(path: &Path, want: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let read = std::fs::read_to_string(path).ok();
        if read.as_deref() == Some(want) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} read {read:?}, not {want:?}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wake_list_lands_beside_the_socket() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("w.sock");
    let list = dir.path().join("wakeup");
    let (connector, discovery, _handle) = new_test_parts();
    let _stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    // The first step writes the empty list.
    wait_contents(&list, "").await;

    let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while client
        .liveliness()
        .get(keys::daemon_alive("t"))
        .await
        .unwrap()
        .recv_async()
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no daemon liveliness"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let caller = Caller::new(client.clone(), "t");
    let request = rpc::AddTv {
        id: None,
        name: "Den".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(
            json!({ "address": "mock://wake", "local_device": "/dev/cec-wake" }),
        )
        .unwrap(),
    };
    let id = caller
        .call::<manager::AddTv>(&(), &request)
        .await
        .unwrap()
        .id;
    let created: rpc::ProfileCreate =
        serde_json::from_value(json!({ "body": { "name": "Wake", "on_tv_power_on": "wake" } }))
            .unwrap();
    let profile = caller
        .call::<profile::Create>(&(), &created)
        .await
        .unwrap()
        .id;
    caller
        .call::<tv::SetSystemProfile>(
            &id,
            &rpc::SetSystemProfile {
                profile: Some(profile),
            },
        )
        .await
        .unwrap();

    wait_contents(&list, "/dev/cec-wake\n").await;
}
