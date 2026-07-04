//! CEC parity over the wire, driven by the real `CecConnector`/
//! `CecConnection` against the upstream `FakeCecLink` (the `test-link`
//! seam): Connected ≠ power, wake-in-place, no link flap through a
//! power cycle.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_cec::{CecConnector, FakeCecLinkOpener, PowerStatus};
use tvnext_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::Sample;
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

#[tokio::test(flavor = "multi_thread")]
async fn cec_power_cycle_keeps_the_link_up() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("c.sock");
    let connector = CecConnector::with_fake_opener(FakeCecLinkOpener::new(PowerStatus::On));
    let _stack = tvnext_daemon::serve(
        &socket,
        "t",
        vec![tvnext_daemon::VendorReg::new(
            "cec",
            "HDMI CEC",
            Arc::new(connector),
        )],
        None,
    )
    .await
    .unwrap();

    let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .unwrap();
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: "CecTv".into(),
        vendor: "cec".into(),
        managed: true,
        config: json!({ "device": "/dev/cec-test" }),
    })
    .unwrap();
    let id = call_ok(&client, &keys::manager_rpc("t", "add-tv"), body).await;
    let id = Uuid::parse_str(id["id"].as_str().unwrap()).unwrap();

    let sub = client
        .declare_subscriber(keys::tv_all("t"))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();

    // The enrollment dial legitimately publishes link{connected:false}
    // before the first connect — drop-tracking starts once we're up.
    let _ = wait_power(&sub, id, "active").await;
    let mut link_dropped = false;

    // Power off: the CEC bus stays reachable in standby.
    call_ok(&client, &keys::tv_rpc("t", &id, "power-off"), Value::Null).await;
    link_dropped |= wait_power(&sub, id, "standby").await;

    // Power on: wake happens in place on the live handle.
    call_ok(&client, &keys::tv_rpc("t", &id, "power-on"), Value::Null).await;
    link_dropped |= wait_power(&sub, id, "active").await;

    assert!(
        !link_dropped,
        "CEC link must never drop across a power cycle (Connected ≠ power)"
    );
}

async fn call_ok(client: &Session, key: &str, body: Value) -> Value {
    let replies = client
        .get(key.to_string())
        .payload(serde_json::to_vec(&body).unwrap())
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let reply = replies.recv_async().await.expect("rpc reply");
    let sample = reply.result().expect("rpc ok");
    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null)
}

/// Wait for the TV's power state, returning whether the link dropped
/// while waiting.
async fn wait_power(sub: &Sub, id: Uuid, want: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut dropped = false;
    loop {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .expect("timed out waiting for power state")
            .expect("subscriber closed");
        let Some((tv, path)) = keys::parse_tv_path("t", sample.key_expr().as_str()) else {
            continue;
        };
        if tv != id {
            continue;
        }
        let value: Value =
            serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
        if path == "state/link" && value["connected"] == false {
            dropped = true;
        }
        if path == "state/power" && value["state"] == want {
            return dropped;
        }
    }
}
