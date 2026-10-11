//! M2 wire tests: real zenoh sessions over a per-test Unix socket, real
//! daemon stack (world reactor + bridge), mock backend behind it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_core::DiscoveredTv;
use tisvlay_mock::{MockHandle, new_test_parts};
use tisvlay_wire::caller::Caller;
use tisvlay_wire::endpoint::{manager, tv};
use tisvlay_wire::error::ErrorCode;
use tisvlay_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::{Sample, SampleKind};
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

use crate::support::{body, call, raw_refusal, refusal};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

struct Wire {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    stack: tisvlay_daemon::Stack,
    handle: MockHandle,
    client: Session,
    caller: Caller,
    site: String,
}

async fn wire() -> Wire {
    let dir = tisvlay_test::socket_tempdir();
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
        caller: Caller::new(client.clone(), "t"),
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

fn add_body(name: &str, address: &str) -> rpc::AddTv {
    rpc::AddTv {
        id: None,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": address })).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn add_tv_then_late_client_hydrates_via_history() {
    let w = wire().await;
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Den", "mock://den"))
        .await
        .expect("add-tv ok");
    let id = ok.id;

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
    let err = call::<tv::SetVolume>(&w.caller, &Uuid::new_v4(), &body(json!({ "level": 5 })))
        .await
        .expect_err("unknown tv must fail");
    assert_eq!(refusal(&err).code, ErrorCode::TvNotFound);

    let err = raw_refusal(
        &w.client,
        &keys::manager_rpc(&w.site, "add-tv"),
        json!({ "nonsense": true }),
    )
    .await;
    assert_eq!(err["code"], "bad-request");
    let err = raw_refusal(
        &w.client,
        &keys::manager_rpc(&w.site, "teleport"),
        Value::Null,
    )
    .await;
    assert_eq!(err["code"], "unknown-rpc");
}

#[tokio::test(flavor = "multi_thread")]
async fn volume_rpc_lands_as_state_publish() {
    let w = wire().await;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Den", "mock://vol"))
        .await
        .expect("add-tv ok");
    let id = ok.id;

    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call::<tv::SetVolume>(&w.caller, &id, &body(json!({ "level": 55 })))
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

/// The rpcs no other suite reaches, each through its typed endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn remaining_rpcs_answer_typed() {
    let w = wire().await;
    let info = call::<manager::Info>(&w.caller, &(), &())
        .await
        .expect("info ok");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.state_path, "", "an ephemeral daemon has no store");

    // A TEST-NET address the ARP cache can't know: the hint is empty.
    let request = rpc::Enrich {
        vendor: "mock".into(),
        ip: "192.0.2.1".into(),
    };
    let hint = call::<manager::Enrich>(&w.caller, &(), &request)
        .await
        .expect("enrich ok");
    assert_eq!(hint.mac, None);

    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    let id = call::<manager::AddTv>(&w.caller, &(), &add_body("Den", "mock://rest"))
        .await
        .expect("add-tv ok")
        .id;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;
    call::<tv::SetMuted>(&w.caller, &id, &rpc::SetMuted { muted: true })
        .await
        .expect("set-muted ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/volume")
            && payload_json(s)["muted"] == true
    })
    .await;
    // Reconnecting a connected TV is a no-op ack.
    call::<tv::Reconnect>(&w.caller, &id, &())
        .await
        .expect("reconnect ok");
}

/// P1's exit criterion: a daemon restart keeps its TVs. The store file is
/// written debounced+atomic; a second stack replaying it (the `run()` boot
/// path) hydrates the same TV for a late client.
#[tokio::test(flavor = "multi_thread")]
async fn restart_preserves_tvs_via_store() {
    let dir = tisvlay_test::socket_tempdir();
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
    let caller1 = Caller::new(client1.clone(), "t");
    let ok = call::<manager::AddTv>(&caller1, &(), &add_body("Persisted", "mock://persist"))
        .await
        .expect("add-tv ok");
    let id = ok.id;

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
async fn settings_rpcs_publish_and_unmanage_keeps_the_link() {
    let w = wire().await;
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Old Name", "mock://settings"))
        .await
        .expect("add-tv ok");
    let id = ok.id;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call::<tv::SetName>(&w.caller, &id, &body(json!({ "name": "New Name" })))
        .await
        .expect("set-name ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "meta/identity")
            && payload_json(s)["name"] == "New Name"
    })
    .await;

    // Managed gates automation, not connectivity: unmanaging turns
    // the automation flag off while the link stays up (state remains
    // live and manual control still works). If unmanage dropped the
    // link, a link{connected:false} would precede the meta publish —
    // track that in the same drain.
    call::<tv::SetManaged>(&w.caller, &id, &body(json!({ "managed": false })))
        .await
        .expect("set-managed ok");
    let mut link_dropped = false;
    recv_until(&sub, |s| {
        let Some((tv, path)) = keys::parse_tv_path(&w.site, s.key_expr().as_str()) else {
            return false;
        };
        if tv != id {
            return false;
        }
        let value = payload_json(s);
        if path == "state/link" && value["connected"] == false {
            link_dropped = true;
        }
        path == "meta/config" && value["managed"] == false && value["automated"] == false
    })
    .await;
    assert!(
        !link_dropped,
        "unmanaging must not disconnect the TV (managed gates automation only)"
    );

    // Manual control still works while unmanaged.
    call::<tv::SetVolume>(&w.caller, &id, &body(json!({ "level": 33 })))
        .await
        .expect("set-volume ok on an unmanaged TV");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/volume")
            && payload_json(s)["level"] == 33
    })
    .await;

    // Managing again just flips automation back on.
    call::<tv::SetManaged>(&w.caller, &id, &body(json!({ "managed": true })))
        .await
        .expect("set-managed ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "meta/config")
            && payload_json(s)["managed"] == true
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_backlight_screen_round_trip() {
    let w = wire().await;
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Den", "mock://ibs"))
        .await
        .expect("add-tv ok");
    let id = ok.id;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call::<tv::SwitchInput>(&w.caller, &id, &body(json!({ "input": "HDMI_2" })))
        .await
        .expect("switch-input ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/input")
            && payload_json(s)["current"] == "HDMI_2"
    })
    .await;

    call::<tv::SetBacklight>(&w.caller, &id, &body(json!({ "level": 42 })))
        .await
        .expect("set-backlight ok");
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/backlight")
            && payload_json(s)["level"] == 42
    })
    .await;

    call::<tv::SetScreen>(&w.caller, &id, &body(json!({ "on": false })))
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
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Movable", "mock://before"))
        .await
        .expect("add-tv ok");
    let id = ok.id;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    let config = call::<tv::GetConfig>(&w.caller, &id, &())
        .await
        .expect("get-config ok");
    assert_eq!(config.name, "Movable");
    assert_eq!(config.config["address"], "mock://before");

    call::<tv::UpdateConfig>(
        &w.caller,
        &id,
        &body(json!({ "address": "mock://after", "name": "Moved" })),
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
    let config = call::<tv::GetConfig>(&w.caller, &id, &())
        .await
        .expect("get-config ok");
    assert_eq!(config.name, "Moved");
    assert_eq!(config.config["address"], "mock://after");

    // The connector-derived display state tracked the patch: a late
    // subscriber hydrates an identity whose primary descriptor is the
    // new address, and meta/vendor rows carrying the schema-ordered,
    // labeled projection (never the alphabetized config object).
    let late = zenoh::open(bootstrap::client_config(&w.socket).unwrap())
        .await
        .unwrap();
    let sub = history_sub(&late, keys::tv_all(&w.site)).await;
    let mut primary_seen = false;
    let mut rows_seen = false;
    recv_until(&sub, |sample| {
        if let Some((tv, path)) = keys::parse_tv_path(&w.site, sample.key_expr().as_str())
            && tv == id
        {
            let value = payload_json(sample);
            match path {
                "meta/identity" if value["primary"] == "mock://after" => primary_seen = true,
                "meta/vendor"
                    if value["config"]
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|r| r[1] == "mock://after")) =>
                {
                    rows_seen = true;
                }
                _ => {}
            }
        }
        primary_seen && rows_seen
    })
    .await;

    // Immutable and malformed patches are typed errors.
    let err = call::<tv::UpdateConfig>(&w.caller, &id, &body(json!({ "vendor": "lg" })))
        .await
        .expect_err("vendor is immutable");
    assert_eq!(refusal(&err).code, ErrorCode::BadRequest);
}

#[tokio::test(flavor = "multi_thread")]
async fn re_pair_refreshes_credentials_in_place() {
    let w = wire().await;
    let request = rpc::AddTv {
        id: None,
        name: "Repairable".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": "mock://rp", "pairing": "accept" }))
            .unwrap(),
    };
    let ok = call::<manager::Pair>(&w.caller, &(), &request)
        .await
        .expect("pair ok");
    let id = ok.id;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    let progress = history_sub(&w.client, keys::pairing_progress(&w.site, &id)).await;
    let started = call::<tv::RePair>(&w.caller, &id, &())
        .await
        .expect("re-pair ok");
    let attempt = started.attempt.to_string();
    assert_ne!(
        started.attempt, ok.attempt,
        "each session mints its own attempt"
    );
    // The progress cache may replay the original session's `complete`
    // ahead of (or, once the re-pair's live samples win the race, never
    // alongside) the re-pair's — only the re-pair's attempt ends the wait.
    recv_until(&progress, |s| {
        let v = payload_json(s);
        v["phase"] == "complete" && v["attempt"] == attempt
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

    // One query, one reply per finding (consolidation off in the caller).
    let request = rpc::Discover {
        vendor: Some("mock".into()),
        timeout_ms: Some(200),
    };
    let found = w
        .caller
        .call_many::<manager::Discover>(&(), &request)
        .await
        .unwrap();
    let mut names: Vec<String> = found.into_iter().map(|tv| tv.name).collect();
    names.sort();
    assert_eq!(names, ["Alpha", "Beta"]);

    let request = rpc::Discover {
        vendor: Some("nope".into()),
        timeout_ms: None,
    };
    let err = w
        .caller
        .call_many::<manager::Discover>(&(), &request)
        .await
        .unwrap_err();
    assert_eq!(refusal(&err).code, ErrorCode::UnknownVendor);
}

/// Full loop: sysfs DPMS flip → watcher → world system event → stock
/// profile → mock backend → state publish on the wire.
#[tokio::test(flavor = "multi_thread")]
async fn dpms_transitions_drive_profile_actions_end_to_end() {
    let w = wire().await;
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Den", "mock://dpms"))
        .await
        .expect("add-tv ok");
    let id = ok.id;
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
    // Override-watcher semantics: unscoped events dispatch globally, so
    // the mock TV (which never EDID-matches a connector) still reacts.
    tokio::spawn(tisvlay_daemon::watch_dpms(
        sysfs.path().to_path_buf(),
        "card9-HDMI-A-9".into(),
        false,
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
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Doomed", "mock://rm"))
        .await
        .expect("add-tv ok");
    let id = ok.id;

    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "state/link")
            && payload_json(s)["connected"] == true
    })
    .await;

    call::<tv::Remove>(&w.caller, &id, &())
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
    let request = rpc::AddTv {
        id: None,
        name: "Pairing".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(
            json!({ "address": "mock://pairwire", "pairing": "manual" }),
        )
        .unwrap(),
    };
    let ok = call::<manager::Pair>(&w.caller, &(), &request)
        .await
        .expect("pair accepted");
    let id = ok.id;

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

    call::<manager::CancelPair>(&w.caller, &(), &body(json!({ "id": id })))
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
    let err = call::<manager::AddTv>(
        &w.caller,
        &(),
        &rpc::AddTv {
            id: None,
            name: "Blank".into(),
            vendor: "mock".into(),
            managed: true,
            config: serde_json::from_value(json!({})).unwrap(),
        },
    )
    .await
    .expect_err("missing required vendor field must be rejected");
    assert_eq!(refusal(&err).code, ErrorCode::InvalidField);
    assert_eq!(refusal(&err).message, "address: required");

    // A valid TV, then a patch blanking the required field. The bridge
    // learns the TV's vendor from the identity publish, so wait for it
    // before patching.
    let ok = call::<manager::AddTv>(&w.caller, &(), &add_body("Valid", "mock://valid"))
        .await
        .expect("add-tv ok");
    let id = ok.id;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    recv_until(&sub, |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, path)| tv == id && path == "meta/identity")
    })
    .await;

    let err = call::<tv::UpdateConfig>(&w.caller, &id, &body(json!({ "address": "  " })))
        .await
        .expect_err("blanking a required vendor field must be rejected");
    assert_eq!(refusal(&err).code, ErrorCode::InvalidField);
    assert_eq!(refusal(&err).message, "address: required");

    // Untouched required fields don't block partial patches.
    call::<tv::UpdateConfig>(&w.caller, &id, &body(json!({ "name": "Still Valid" })))
        .await
        .expect("partial patch without the required field is fine");
}

/// End to end over the wire: a pairing whose device reports its link MAC
/// stores it (canonical form) when none was configured, and the terminal
/// sample tells the client what happened.
#[tokio::test(flavor = "multi_thread")]
async fn pair_fills_the_mac_the_device_reports() {
    let w = wire().await;
    let request = rpc::AddTv {
        id: None,
        name: "Routed".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({
            "address": "mock://routed",
            "pairing": "accept",
            "link_mac": "84-90-0a-63-a9-94",
        }))
        .unwrap(),
    };
    let ok = call::<manager::Pair>(&w.caller, &(), &request)
        .await
        .expect("pair ok");
    let id = ok.id;
    let progress = history_sub(&w.client, keys::pairing_progress(&w.site, &id)).await;
    let seen = recv_until(&progress, |s| {
        let v = payload_json(s);
        v["phase"] == "complete" && v["attempt"] == ok.attempt.to_string()
    })
    .await;
    let complete = payload_json(seen.last().unwrap());
    assert_eq!(
        complete["link_mac"],
        json!({ "reported": "84:90:0A:63:A9:94", "outcome": { "kind": "filled" } })
    );

    let config = call::<tv::GetConfig>(&w.caller, &id, &())
        .await
        .expect("get-config ok");
    assert_eq!(config.config["mac"], "84:90:0A:63:A9:94");
}
