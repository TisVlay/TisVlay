//! Consumed-D-Bus adapter test: a fake logind on a private bus drives the
//! world's sleep/resume actions end to end (signal → system event → stock
//! profile → mock backend → state publish on the wire).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_test::PrivateBus;
use tisvlay_test::fake_logind::FakeLogind;
use tvnext_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::bytes::Encoding;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

#[tokio::test(flavor = "multi_thread")]
async fn logind_sleep_resume_drive_profile_actions() {
    let bus = PrivateBus::shared();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("l.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let stack = tvnext_daemon::serve(
        &socket,
        "t",
        vec![("mock", Arc::new(connector))],
        vec![("mock", Arc::new(discovery))],
        None,
    )
    .await
    .unwrap();
    let watcher_conn = bus.connect().await.expect("watcher conn");
    let world = stack.world.clone();
    tokio::spawn(async move {
        let _ = tvnext_daemon::watch_logind(watcher_conn, world).await;
    });

    let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .unwrap();
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: "Den".into(),
        vendor: "mock".into(),
        managed: true,
        config: json!({ "address": "mock://logind" }),
    })
    .unwrap();
    let replies = client
        .get(keys::manager_rpc("t", "add-tv"))
        .payload(serde_json::to_vec(&body).unwrap())
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let reply = replies.recv_async().await.expect("add reply");
    let ok: Value =
        serde_json::from_slice(&reply.result().expect("ok").payload().to_bytes()).unwrap();
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    let sub = client
        .declare_subscriber(keys::tv_all("t"))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();
    wait_power(&sub, id, "active").await;

    // Give the watcher a beat to be subscribed before emitting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.emit_prepare_for_sleep(true).await.unwrap();
    wait_power(&sub, id, "screenoff").await;

    fake.emit_prepare_for_sleep(false).await.unwrap();
    wait_power(&sub, id, "active").await;
}

async fn wait_power(
    sub: &zenoh_ext::AdvancedSubscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>,
    id: Uuid,
    want: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .expect("timed out waiting for power state")
            .expect("subscriber closed");
        if let Some((tv, path)) = keys::parse_tv_path("t", sample.key_expr().as_str())
            && tv == id
            && path == "state/power"
        {
            let value: Value =
                serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
            if value["state"] == want {
                return;
            }
        }
    }
}
