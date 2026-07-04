//! M2 wire tests: real zenoh sessions over a per-test Unix socket, real
//! daemon stack (world reactor + bridge), mock backend behind it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_core::DiscoveredTv;
use tisvlay_mock::{MockHandle, new_test_parts};
use tisvlay_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::{Sample, SampleKind};
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

struct Wire {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    stack: tisvlay_daemon::Stack,
    handle: MockHandle,
    client: Session,
    site: String,
}

async fn wire() -> Wire {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("w.sock");
    let (connector, discovery, handle) = new_test_parts();
    let stack = tisvlay_daemon::serve(
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
    await_daemon_visible(&client, "t").await;
    Wire {
        _dir: dir,
        socket,
        stack,
        handle,
        client,
        site: "t".into(),
    }
}

/// Wait until the daemon's liveliness token is visible from `client`.
/// `serve` declares the token only after its rpc queryables, and
/// declarations reach a late-joining session in order — so a visible
/// token means every rpc is ready and raw gets can't hit the
/// no-queryable instant-empty-reply case.
async fn await_daemon_visible(client: &Session, site: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replies = client
            .liveliness()
            .get(keys::daemon_alive(site))
            .await
            .unwrap();
        if replies.recv_async().await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon liveliness never became visible"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn history_sub(session: &Session, key: String) -> Sub {
    session
        .declare_subscriber(key)
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap()
}

/// One RPC round-trip; `Err` carries the decoded typed-error payload.
/// A get that matches zero queryables completes instantly with no
/// reply — the bridge may still be declaring its queryables right
/// after `serve` returns — so the no-reply case retries briefly.
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

/// Receive samples until `pred` matches, panicking after 10 s.
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
                        .map(|s: &Sample| s.key_expr().to_string())
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

fn payload_json(sample: &Sample) -> Value {
    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null)
}

fn add_body(name: &str, address: &str) -> Value {
    serde_json::to_value(rpc::AddTv {
        id: None,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        config: json!({ "address": address }),
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn add_tv_then_late_client_hydrates_via_history() {
    let w = wire().await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Den", "mock://den"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().expect("reply carries id")).unwrap();

    // A brand-new session, opened after the fact, hydrates from the
    // publisher caches through subscriber history (plus live samples for
    // whatever is still in flight).
    let late = zenoh::open(bootstrap::client_config(&w.socket).unwrap())
        .await
        .unwrap();
    let sub = history_sub(&late, keys::tv_all(&w.site)).await;
    let mut name_seen = false;
    let mut connected_seen = false;
    recv_until(&sub, |sample| {
        if let Some((tv, path)) = keys::parse_tv_path(&w.site, sample.key_expr().as_str())
            && tv == id
        {
            let value = payload_json(sample);
            match path {
                "meta/identity" if value["name"] == "Den" => name_seen = true,
                "state/link" if value["connected"] == true => connected_seen = true,
                _ => {}
            }
        }
        name_seen && connected_seen
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_errors_are_typed() {
    let w = wire().await;
    let err = call(
        &w.client,
        &keys::tv_rpc(&w.site, &Uuid::new_v4(), "set-volume"),
        json!({ "level": 5 }),
    )
    .await
    .expect_err("unknown tv must fail");
    assert_eq!(err["code"], "tv-not-found");

    let err = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        json!({ "nonsense": true }),
    )
    .await
    .expect_err("malformed body must fail");
    assert_eq!(err["code"], "bad-request");
}

#[tokio::test(flavor = "multi_thread")]
async fn volume_rpc_lands_as_state_publish() {
    let w = wire().await;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Den", "mock://vol"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "set-volume"),
        json!({ "level": 55 }),
    )
    .await
    .expect("set-volume ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/volume")
            && payload_json(s)["level"] == 55
    })
    .await;
    // The push originated from the backend, proving the full loop:
    // rpc → world → connection task → mock → subscription → publish.
}

/// P1's exit criterion: a daemon restart keeps its TVs. The store file is
/// written debounced+atomic; a second stack replaying it (the `run()` boot
/// path) hydrates the same TV for a late client.
#[tokio::test(flavor = "multi_thread")]
async fn restart_preserves_tvs_via_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("daemon.toml");
    let socket1 = dir.path().join("a.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let stack1 = tisvlay_daemon::serve(
        &socket1,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        Some(&store),
    )
    .await
    .unwrap();
    let client1 = zenoh::open(bootstrap::client_config(&socket1).unwrap())
        .await
        .unwrap();
    let ok = call(
        &client1,
        &keys::manager_rpc("t", "add-tv"),
        add_body("Persisted", "mock://persist"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    // Wait for the debounced write to land.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&store).is_ok_and(|t| t.contains(&id.to_string())) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "store file never appeared"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(client1);
    stack1.daemon.close().await.unwrap();

    // Second life: fresh stack + mock, same store, replayed like run() does.
    let socket2 = dir.path().join("b.sock");
    let (connector2, discovery2, _handle2) = new_test_parts();
    let stack2 = tisvlay_daemon::serve(
        &socket2,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector2))
                .with_discovery(Arc::new(discovery2)),
        ],
        Some(&store),
    )
    .await
    .unwrap();
    for record in tisvlay_daemon::load_store(&store).unwrap().tv {
        stack2.world.command(tisvlay_world::Command::AddTv {
            record: Box::new(record),
            reply: None,
        });
    }
    let late = zenoh::open(bootstrap::client_config(&socket2).unwrap())
        .await
        .unwrap();
    let sub = history_sub(&late, keys::tv_all("t")).await;
    let mut name_seen = false;
    let mut connected_seen = false;
    recv_until(&sub, |sample| {
        if let Some((tv, path)) = keys::parse_tv_path("t", sample.key_expr().as_str())
            && tv == id
        {
            let value = payload_json(sample);
            match path {
                "meta/identity" if value["name"] == "Persisted" => name_seen = true,
                "state/link" if value["connected"] == true => connected_seen = true,
                _ => {}
            }
        }
        name_seen && connected_seen
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_rpcs_publish_and_unmanage_disconnects() {
    let w = wire().await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Old Name", "mock://settings"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "set-name"),
        json!({ "name": "New Name" }),
    )
    .await
    .expect("set-name ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "meta/identity")
            && payload_json(s)["name"] == "New Name"
    })
    .await;

    // Unmanaging drops the connection.
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "set-managed"),
        json!({ "managed": false }),
    )
    .await
    .expect("set-managed ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == false
    })
    .await;

    // Managing again reconnects.
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "set-managed"),
        json!({ "managed": true }),
    )
    .await
    .expect("set-managed ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_backlight_screen_round_trip() {
    let w = wire().await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Den", "mock://ibs"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "switch-input"),
        json!({ "input": "HDMI_2" }),
    )
    .await
    .expect("switch-input ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/input")
            && payload_json(s)["current"] == "HDMI_2"
    })
    .await;

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "set-backlight"),
        json!({ "level": 42 }),
    )
    .await
    .expect("set-backlight ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/backlight")
            && payload_json(s)["level"] == 42
    })
    .await;

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "set-screen"),
        json!({ "on": false }),
    )
    .await
    .expect("set-screen ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn get_config_reads_and_update_config_moves_the_tv() {
    let w = wire().await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Movable", "mock://before"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    let config = call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "get-config"),
        Value::Null,
    )
    .await
    .expect("get-config ok");
    assert_eq!(config["name"], "Movable");
    assert_eq!(config["config"]["address"], "mock://before");

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "update-config"),
        json!({ "address": "mock://after", "name": "Moved" }),
    )
    .await
    .expect("update-config ok");
    // Vendor change forces a reconnect against the new address.
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == false
    })
    .await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;
    assert!(w.handle.connect_call_count("mock://after") >= 1);
    let config = call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "get-config"),
        Value::Null,
    )
    .await
    .expect("get-config ok");
    assert_eq!(config["name"], "Moved");
    assert_eq!(config["config"]["address"], "mock://after");

    // Immutable and malformed patches are typed errors.
    let err = call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "update-config"),
        json!({ "vendor": "lg" }),
    )
    .await
    .expect_err("vendor is immutable");
    assert_eq!(err["code"], "bad-request");
}

#[tokio::test(flavor = "multi_thread")]
async fn re_pair_refreshes_credentials_in_place() {
    let w = wire().await;
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: "Repairable".into(),
        vendor: "mock".into(),
        managed: true,
        config: json!({ "address": "mock://rp", "pairing": "accept" }),
    })
    .unwrap();
    let ok = call(&w.client, &keys::manager_rpc(&w.site, "pair"), body)
        .await
        .expect("pair ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    let progress = history_sub(&w.client, keys::pairing_progress(&w.site, &id)).await;
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "re-pair"),
        Value::Null,
    )
    .await
    .expect("re-pair ok");
    // The progress cache replays the original session's `complete` too —
    // the re-pair's is the second one.
    let mut completes = 0;
    recv_until(&progress, |s| {
        if payload_json(s)["phase"] == "complete" {
            completes += 1;
        }
        completes >= 2
    })
    .await;
    // Credentials refreshed in place: same uuid, reconnected.
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;
}

/// The M2b shape in action: one discover query, one reply per finding.
#[tokio::test(flavor = "multi_thread")]
async fn discover_streams_findings_as_multiple_replies() {
    let w = wire().await;
    w.handle
        .publish_tv(DiscoveredTv::new("tv-a", "Alpha", "mock://a", "mock"));
    w.handle
        .publish_tv(DiscoveredTv::new("tv-b", "Beta", "mock://b", "mock"));

    // Consolidation off: discovery replies share the query's key, and the
    // default mode would collapse them to the latest one.
    let replies = w
        .client
        .get(keys::manager_rpc(&w.site, "discover"))
        .payload(serde_json::to_vec(&json!({ "vendor": "mock", "timeout_ms": 200 })).unwrap())
        .encoding(Encoding::APPLICATION_JSON)
        .consolidation(zenoh::query::ConsolidationMode::None)
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let mut names = Vec::new();
    while let Ok(reply) = replies.recv_async().await {
        let sample = reply.result().expect("discover replies are ok samples");
        names.push(payload_json(sample)["name"].as_str().unwrap().to_string());
    }
    names.sort();
    assert_eq!(names, ["Alpha", "Beta"]);

    let replies = w
        .client
        .get(keys::manager_rpc(&w.site, "discover"))
        .payload(serde_json::to_vec(&json!({ "vendor": "nope" })).unwrap())
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let reply = replies.recv_async().await.expect("one reply");
    let err = reply.result().expect_err("unknown vendor must err");
    let value: Value = serde_json::from_slice(&err.payload().to_bytes()).unwrap();
    assert_eq!(value["code"], "unknown-vendor");
}

/// Full loop: sysfs DPMS flip → watcher → world system event → stock
/// profile → mock backend → state publish on the wire.
#[tokio::test(flavor = "multi_thread")]
async fn dpms_transitions_drive_profile_actions_end_to_end() {
    let w = wire().await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Den", "mock://dpms"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    let sysfs = tempfile::tempdir().unwrap();
    let connector_dir = sysfs.path().join("card9-HDMI-A-9");
    std::fs::create_dir_all(&connector_dir).unwrap();
    std::fs::write(connector_dir.join("status"), "connected\n").unwrap();
    std::fs::write(connector_dir.join("dpms"), "On\n").unwrap();
    tokio::spawn(tisvlay_daemon::watch_dpms(
        sysfs.path().to_path_buf(),
        "card9-HDMI-A-9".into(),
        Duration::from_millis(25),
        w.stack.world.clone(),
    ));
    // Let the watcher take its baseline reading before flipping.
    tokio::time::sleep(Duration::from_millis(100)).await;

    std::fs::write(connector_dir.join("dpms"), "Off\n").unwrap();
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    std::fs::write(connector_dir.join("dpms"), "On\n").unwrap();
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/power")
            && payload_json(s)["state"] == "active"
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_emits_deletes_and_late_joiners_see_nothing() {
    let w = wire().await;
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Doomed", "mock://rm"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "remove"),
        Value::Null,
    )
    .await
    .expect("remove ok");
    // Live subscribers see the tombstones.
    recv_until(&sub, |s| {
        s.kind() == SampleKind::Delete
            && keys::parse_tv_path(&w.site, s.key_expr().as_str()).is_some_and(|(tv, _)| tv == id)
    })
    .await;

    // A late joiner hydrates nothing for the removed TV — its publishers
    // (and their caches) are gone.
    let late = zenoh::open(bootstrap::client_config(&w.socket).unwrap())
        .await
        .unwrap();
    let late_sub = history_sub(&late, keys::tv_all(&w.site)).await;
    while let Ok(Ok(sample)) =
        tokio::time::timeout(Duration::from_millis(600), late_sub.recv_async()).await
    {
        if sample.kind() == SampleKind::Put
            && let Some((tv, path)) = keys::parse_tv_path(&w.site, sample.key_expr().as_str())
        {
            assert_ne!(tv, id, "removed TV leaked {path} to a late joiner");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_progress_liveliness_and_cancel_over_wire() {
    let w = wire().await;
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: "Pairing".into(),
        vendor: "mock".into(),
        managed: true,
        config: json!({ "address": "mock://pairwire", "pairing": "manual" }),
    })
    .unwrap();
    let ok = call(&w.client, &keys::manager_rpc(&w.site, "pair"), body)
        .await
        .expect("pair accepted");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    // Progress replays from the deep pairing cache.
    let sub = history_sub(&w.client, keys::pairing_progress(&w.site, &id)).await;
    recv_until(&sub, |s| payload_json(s)["phase"] == "awaiting_tv").await;

    // The session is alive on the liveliness plane.
    let replies = w
        .client
        .liveliness()
        .get(keys::alive_pairing(&w.site, &id).as_str())
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        replies.recv_async().await.is_ok_and(|r| r.result().is_ok()),
        "pairing liveliness token must be present mid-session"
    );

    call(
        &w.client,
        &keys::manager_rpc(&w.site, "cancel-pair"),
        json!({ "id": id }),
    )
    .await
    .expect("cancel ok");
    let seen = recv_until(&sub, |s| payload_json(s)["phase"] == "failed").await;
    let failed = payload_json(seen.last().unwrap());
    assert_eq!(failed["code"], "pairing.cancelled");

    // Token gone after the terminal phase.
    let replies = w
        .client
        .liveliness()
        .get(keys::alive_pairing(&w.site, &id).as_str())
        .timeout(Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        replies.recv_async().await.is_err(),
        "pairing liveliness token must drop on terminal phase"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_field_validation_rejects_blank_required_fields() {
    let w = wire().await;

    // add-tv with the mock's required `address` field absent: rejected
    // before the world sees it, with the field routed in the message.
    let err = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        serde_json::to_value(rpc::AddTv {
            id: None,
            name: "Blank".into(),
            vendor: "mock".into(),
            managed: true,
            config: json!({}),
        })
        .unwrap(),
    )
    .await
    .expect_err("missing required vendor field must be rejected");
    assert_eq!(err["code"], "invalid-field");
    assert_eq!(err["message"], "address: required");

    // A valid TV, then a patch blanking the required field. The bridge
    // learns the TV's vendor from the identity publish, so wait for it
    // before patching.
    let ok = call(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        add_body("Valid", "mock://valid"),
    )
    .await
    .expect("add-tv ok");
    let id = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "meta/identity")
    })
    .await;

    let err = call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "update-config"),
        json!({ "address": "  " }),
    )
    .await
    .expect_err("blanking a required vendor field must be rejected");
    assert_eq!(err["code"], "invalid-field");
    assert_eq!(err["message"], "address: required");

    // Untouched required fields don't block partial patches.
    call(
        &w.client,
        &keys::tv_rpc(&w.site, &id, "update-config"),
        json!({ "name": "Still Valid" }),
    )
    .await
    .expect("partial patch without the required field is fine");
}
