//! CEC parity over the wire, driven by the real `CecConnector`/
//! `CecConnection` against the upstream `FakeCecLink` (the `test-link`
//! seam): Connected ≠ power, wake-in-place, no link flap through a
//! power cycle.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_cec::{CecConnector, FakeCecLinkOpener, PowerStatus};
use tisvlay_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::Sample;
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

#[tokio::test(flavor = "multi_thread")]
async fn cec_power_cycle_keeps_the_link_up() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("c.sock");
    let connector = CecConnector::with_fake_opener(FakeCecLinkOpener::new(PowerStatus::On));
    let _stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![tisvlay_daemon::VendorReg::new(
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
    // before the first connect, and per-key publishers give no cross-key
    // ordering — that sample can even land after the first power
    // reading. `wait_power` tracks the link state across the whole
    // drain: a drop only counts once link{true} has been seen.
    let mut link_was_up = false;
    let _ = wait_power(&sub, id, "active", &mut link_was_up).await;

    // Power off: the CEC bus stays reachable in standby.
    call_ok(&client, &keys::tv_rpc("t", &id, "power-off"), Value::Null).await;
    let mut link_dropped = wait_power(&sub, id, "standby", &mut link_was_up).await;

    // Power on: wake happens in place on the live handle.
    call_ok(&client, &keys::tv_rpc("t", &id, "power-on"), json!({})).await;
    link_dropped |= wait_power(&sub, id, "active", &mut link_was_up).await;

    assert!(
        !link_dropped,
        "CEC link must never drop across a power cycle (Connected ≠ power)"
    );
}

/// A get that matches zero queryables completes instantly with no
/// reply — the served side may still be declaring its queryables —
/// so the no-reply case retries briefly.
async fn call_ok(client: &Session, key: &str, body: Value) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replies = client
            .get(key.to_string())
            .payload(serde_json::to_vec(&body).unwrap())
            .encoding(Encoding::APPLICATION_JSON)
            .timeout(Duration::from_secs(5))
            .await
            .unwrap();
        let reply = match replies.recv_async().await {
            Ok(reply) => reply,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("rpc got no reply: {e}"),
        };
        let sample = reply.result().expect("rpc ok");
        return serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
    }
}

/// Wait for the TV's power state, returning whether the link dropped
/// (a `connected:false` after a seen `connected:true`) while waiting.
async fn wait_power(sub: &Sub, id: Uuid, want: &str, link_was_up: &mut bool) -> bool {
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
        if path == "state/link" {
            if value["connected"] == true {
                *link_was_up = true;
            } else if *link_was_up {
                dropped = true;
            }
        }
        if path == "state/power" && value["state"] == want {
            return dropped;
        }
    }
}

/// End to end over the wire: pairing a CEC device the daemon can't
/// open fails with the device reason code and the node path, and the
/// TV rolls back — the wizard shows the group/udev fix instead of a
/// paired-but-dead row.
#[tokio::test(flavor = "multi_thread")]
async fn cec_pair_fails_typed_when_device_is_inaccessible() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("p.sock");
    let connector = CecConnector::with_fake_opener(FakeCecLinkOpener::permission_denied());
    let _stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![tisvlay_daemon::VendorReg::new(
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

    // Progress subscription first (history) so no phase is missed.
    let progress_sub = client
        .declare_subscriber("tisvlay/t/pairing/*/progress")
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();

    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: "CecDenied".into(),
        vendor: "cec".into(),
        managed: true,
        config: json!({ "device": "/dev/cec-denied" }),
    })
    .unwrap();
    let ok = call_ok(&client, &keys::manager_rpc("t", "pair"), body).await;
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let failed = loop {
        let sample = tokio::time::timeout_at(deadline, progress_sub.recv_async())
            .await
            .expect("timed out waiting for pairing terminal")
            .expect("subscriber closed");
        let value: Value =
            serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
        assert_ne!(
            value["phase"], "complete",
            "an unopenable device must not pair"
        );
        if value["phase"] == "failed" {
            break value;
        }
    };
    assert_eq!(failed["code"], "device.permission-denied");
    assert_eq!(failed["path"], "/dev/cec-denied");

    // Rollback: the TV is gone from the catalog.
    let err = call_err(&client, &keys::tv_rpc("t", &id, "get-config"), Value::Null).await;
    assert_eq!(err["code"], "tv-not-found");
}

/// Like `call_ok`, but the rpc must fail — returns the typed error
/// payload. Retries the no-reply case (queryable declaration race).
async fn call_err(client: &Session, key: &str, body: Value) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replies = client
            .get(key.to_string())
            .payload(serde_json::to_vec(&body).unwrap())
            .encoding(Encoding::APPLICATION_JSON)
            .timeout(Duration::from_secs(5))
            .await
            .unwrap();
        let reply = match replies.recv_async().await {
            Ok(reply) => reply,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("rpc got no reply: {e}"),
        };
        let err = reply.result().expect_err("rpc must fail");
        return serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null);
    }
}
