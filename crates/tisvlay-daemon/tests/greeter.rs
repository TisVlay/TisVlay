//! P4 daemon-side wire tests: the greeter contract (timeout key + idle
//! puts driving system-profile actions) and the `automate_when` gate
//! (published `automated` flag + suppressed dispatch).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::Sample;
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

struct Wire {
    _dir: tempfile::TempDir,
    client: Session,
    site: String,
}

async fn wire() -> Wire {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("g.sock");
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
    let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .unwrap();
    Wire {
        _dir: dir,
        client,
        site: "t".into(),
    }
}

async fn history_sub(session: &Session, key: String) -> Sub {
    session
        .declare_subscriber(key)
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap()
}

/// A get that matches zero queryables completes instantly with no
/// reply — the served side may still be declaring its queryables —
/// so the no-reply case retries briefly.
async fn call(client: &Session, key: &str, body: Value) -> Result<Value, Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replies = client
            .get(key.to_string())
            .payload(serde_json::to_vec(&body).unwrap())
            .encoding(Encoding::APPLICATION_JSON)
            .timeout(Duration::from_secs(5))
            .await
            .unwrap();
        let reply = match tokio::time::timeout(Duration::from_secs(5), replies.recv_async())
            .await
            .expect("rpc timed out")
        {
            Ok(reply) => reply,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("rpc got no reply: {e}"),
        };
        return match reply.result() {
            Ok(sample) => {
                Ok(serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null))
            }
            Err(err) => {
                Err(serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null))
            }
        };
    }
}

async fn recv_until(sub: &Sub, mut pred: impl FnMut(&Sample) -> bool) -> Vec<Sample> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out; saw: {:?}",
                    seen.iter()
                        .map(|s: &Sample| format!("{} {}", s.key_expr(), payload_json(s)))
                        .collect::<Vec<_>>()
                )
            })
            .expect("subscriber closed");
        let done = pred(&sample);
        seen.push(sample);
        if done {
            return seen;
        }
    }
}

async fn assert_no_sample(sub: &Sub, window: Duration, mut pred: impl FnMut(&Sample) -> bool) {
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Ok(sample)) = tokio::time::timeout_at(deadline, sub.recv_async()).await {
        assert!(!pred(&sample), "unexpected sample on {}", sample.key_expr());
    }
}

fn payload_json(sample: &Sample) -> Value {
    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null)
}

async fn add_tv(w: &Wire, name: &str, address: &str) -> Uuid {
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        config: json!({ "address": address }),
    })
    .unwrap();
    let ok = call(&w.client, &keys::manager_rpc(&w.site, "add-tv"), body)
        .await
        .expect("add-tv ok");
    Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap()
}

async fn wait_tv_state(sub: &Sub, site: &str, id: Uuid, path: &str, pred: impl Fn(&Value) -> bool) {
    recv_until(sub, |s| {
        keys::parse_tv_path(site, s.key_expr().as_str())
            .is_some_and(|(tv, p)| tv == id && p == path)
            && pred(&payload_json(s))
    })
    .await;
}

/// Publish a greeter idle transition the way `tisvlay-greeter` does.
async fn put_greeter_idle(w: &Wire, idle: bool) {
    w.client
        .put(
            keys::greeter_idle(&w.site),
            serde_json::to_vec(&json!({ "idle": idle })).unwrap(),
        )
        .encoding(Encoding::APPLICATION_JSON)
        .await
        .unwrap();
}

/// The full greeter loop: the timeout key tracks profile assignments, and
/// idle/resume puts drive the stock `on_idle_timeout`/`on_idle_resume`
/// actions.
#[tokio::test(flavor = "multi_thread")]
async fn greeter_timeout_key_and_idle_dispatch() {
    let w = wire().await;
    let timeout_sub = history_sub(&w.client, keys::greeter_timeout(&w.site)).await;
    recv_until(&timeout_sub, |s| payload_json(s)["seconds"] == 0).await;

    let tv = add_tv(&w, "Hall", "mock://greeter").await;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    wait_tv_state(&sub, &w.site, tv, "state/link", |v| v["connected"] == true).await;

    // An assigned profile with an enabled timer surfaces on the key.
    let ok = call(
        &w.client,
        &keys::profile_rpc(&w.site, "create"),
        json!({ "body": { "name": "Greeter", "idle_timeout_seconds": 300 } }),
    )
    .await
    .expect("create ok");
    let profile = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &tv, "set-system-profile"),
        json!({ "profile": profile }),
    )
    .await
    .expect("assign ok");
    recv_until(&timeout_sub, |s| payload_json(s)["seconds"] == 300).await;

    // Idle blanks (stock on_idle_timeout), resume relights.
    put_greeter_idle(&w, true).await;
    wait_tv_state(&sub, &w.site, tv, "state/power", |v| {
        v["state"] == "screenoff"
    })
    .await;
    put_greeter_idle(&w, false).await;
    wait_tv_state(&sub, &w.site, tv, "state/power", |v| v["state"] == "active").await;

    // Unassigning drops the timer back to disabled.
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &tv, "set-system-profile"),
        json!({}),
    )
    .await
    .expect("unassign ok");
    recv_until(&timeout_sub, |s| payload_json(s)["seconds"] == 0).await;
}

/// The automate_when gate end to end: the published `automated` flag
/// follows input conditions, gated TVs ignore system events, and invalid
/// gate values are rejected at the rpc boundary.
#[tokio::test(flavor = "multi_thread")]
async fn automate_when_gates_dispatch_and_meta_flag() {
    let w = wire().await;
    let tv = add_tv(&w, "Gated", "mock://gate").await;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    // One drain for both conditions — `automated` starts true and never
    // republishes, so a sequential wait would consume its only sample.
    let (mut connected, mut automated) = (false, false);
    recv_until(&sub, |s| {
        if let Some((id, path)) = keys::parse_tv_path(&w.site, s.key_expr().as_str())
            && id == tv
        {
            let value = payload_json(s);
            connected |= path == "state/link" && value["connected"] == true;
            automated |= path == "meta/config" && value["automated"] == true;
        }
        connected && automated
    })
    .await;

    let err = call(
        &w.client,
        &keys::tv_rpc(&w.site, &tv, "set-automate-when"),
        json!({ "when": "sometimes" }),
    )
    .await
    .expect_err("invalid gate value must fail");
    assert_eq!(err["code"], "bad-request");

    // Mock sits on HDMI_1; require the primary input HDMI_2 → gated off.
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &tv, "set-primary-input"),
        json!({ "input": "HDMI_2" }),
    )
    .await
    .expect("set primary ok");
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &tv, "set-automate-when"),
        json!({ "when": "on_primary" }),
    )
    .await
    .expect("set gate ok");
    wait_tv_state(&sub, &w.site, tv, "meta/config", |v| {
        v["automated"] == false
    })
    .await;

    put_greeter_idle(&w, true).await;
    assert_no_sample(&sub, Duration::from_millis(400), |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(id, p)| id == tv && p == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    // Switching onto the primary input re-arms automation.
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &tv, "switch-input"),
        json!({ "input": "HDMI_2" }),
    )
    .await
    .expect("switch ok");
    wait_tv_state(&sub, &w.site, tv, "meta/config", |v| v["automated"] == true).await;
    put_greeter_idle(&w, false).await;
    put_greeter_idle(&w, true).await;
    wait_tv_state(&sub, &w.site, tv, "state/power", |v| {
        v["state"] == "screenoff"
    })
    .await;
}
