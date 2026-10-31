//! P3 wire tests: system-profile CRUD with reference guarding, per-TV
//! profile resolution overriding the stock mapping, discovery sessions
//! (liveliness + streamed findings), and the vendor catalog queryables.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_core::DiscoveredTv;
use tisvlay_mock::{MockHandle, new_test_parts};
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{manager, profile, tv, vendors};
use tisvlay_protocol::error::ErrorCode;
use tisvlay_protocol::{bootstrap, keys, rpc};
use tisvlay_world::{InEvent, SystemEvent};
use uuid::Uuid;
use zenoh::Session;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::{Sample, SampleKind};
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

use crate::support::{body, call, refusal};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

struct Wire {
    _dir: tempfile::TempDir,
    stack: tisvlay_daemon::Stack,
    handle: MockHandle,
    client: Session,
    caller: Caller,
    site: String,
}

async fn wire() -> Wire {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("p.sock");
    let (connector, discovery, handle) = new_test_parts();
    let stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_capabilities(vec!["power".into(), "volume".into()])
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
        stack,
        handle,
        caller: Caller::new(client.clone(), "t"),
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

/// Drain for `window` asserting no sample matches `pred`.
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
    let request = rpc::AddTv {
        id: None,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": address })).unwrap(),
    };
    let ok = call::<manager::AddTv>(&w.caller, &(), &request)
        .await
        .expect("add-tv ok");
    ok.id
}

async fn wait_tv_state(sub: &Sub, site: &str, id: Uuid, path: &str, pred: impl Fn(&Value) -> bool) {
    wait_all_tv_state(sub, site, [id], path, pred).await;
}

/// Wait until every TV in `ids` has matched once — a single drain, so one
/// TV's sample can't be consumed while waiting on another.
async fn wait_all_tv_state<const N: usize>(
    sub: &Sub,
    site: &str,
    ids: [Uuid; N],
    path: &str,
    pred: impl Fn(&Value) -> bool,
) {
    let mut pending: Vec<Uuid> = ids.into();
    recv_until(sub, |s| {
        if let Some((tv, p)) = keys::parse_tv_path(site, s.key_expr().as_str())
            && p == path
            && pending.contains(&tv)
            && pred(&payload_json(s))
        {
            pending.retain(|id| *id != tv);
        }
        pending.is_empty()
    })
    .await;
}

fn profile_wild(site: &str) -> String {
    format!("tisvlay/{site}/profile/system/*")
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_crud_publishes_bodies_and_guards_references() {
    let w = wire().await;
    let profile_id = Uuid::new_v4();

    let ok = call::<profile::Create>(
        &w.caller,
        &(),
        &body(json!({ "id": profile_id, "body": { "name": "Movie", "on_dpms_off": "none" } })),
    )
    .await
    .expect("create ok");
    assert_eq!(ok.id, profile_id);

    let err = call::<profile::Create>(
        &w.caller,
        &(),
        &body(json!({ "id": profile_id, "body": { "name": "Clash" } })),
    )
    .await
    .expect_err("duplicate id must fail");
    assert_eq!(refusal(&err).code, ErrorCode::ProfileExists);

    // Late joiner hydrates the body from the cached publisher.
    let sub = history_sub(&w.client, profile_wild(&w.site)).await;
    recv_until(&sub, |s| {
        s.key_expr().as_str() == keys::profile_system(&w.site, &profile_id)
            && payload_json(s)["name"] == "Movie"
    })
    .await;

    call::<profile::Update>(
        &w.caller,
        &(),
        &body(
            json!({ "id": profile_id, "body": { "name": "Movie Night", "on_sleep": "power_off" } }),
        ),
    )
    .await
    .expect("update ok");
    recv_until(&sub, |s| {
        let body = payload_json(s);
        s.key_expr().as_str() == keys::profile_system(&w.site, &profile_id)
            && body["name"] == "Movie Night"
            && body["on_sleep"] == "power_off"
    })
    .await;

    let ok = call::<profile::Duplicate>(
        &w.caller,
        &(),
        &body(json!({ "id": profile_id, "name": "Copy" })),
    )
    .await
    .expect("duplicate ok");
    let copy_id = ok.id;
    assert_ne!(copy_id, profile_id);
    recv_until(&sub, |s| {
        let body = payload_json(s);
        s.key_expr().as_str() == keys::profile_system(&w.site, &copy_id)
            && body["name"] == "Copy"
            && body["on_sleep"] == "power_off"
    })
    .await;
    call::<profile::Delete>(&w.caller, &(), &body(json!({ "id": copy_id })))
        .await
        .expect("delete copy ok");

    // A referencing TV blocks deletion until unassigned.
    let tv = add_tv(&w, "Den", "mock://prof-crud").await;
    let unknown = Uuid::new_v4();
    let err = call::<tv::SetSystemProfile>(&w.caller, &tv, &body(json!({ "profile": unknown })))
        .await
        .expect_err("assigning an unknown profile must fail");
    assert_eq!(refusal(&err).code, ErrorCode::ProfileNotFound);
    call::<tv::SetSystemProfile>(&w.caller, &tv, &body(json!({ "profile": profile_id })))
        .await
        .expect("assign ok");
    let err = call::<profile::Delete>(&w.caller, &(), &body(json!({ "id": profile_id })))
        .await
        .expect_err("delete while referenced must fail");
    assert_eq!(refusal(&err).code, ErrorCode::ProfileInUse);

    call::<tv::SetSystemProfile>(&w.caller, &tv, &body(json!({})))
        .await
        .expect("unassign ok");
    call::<profile::Delete>(&w.caller, &(), &body(json!({ "id": profile_id })))
        .await
        .expect("delete ok");
    recv_until(&sub, |s| {
        s.kind() == SampleKind::Delete
            && s.key_expr().as_str() == keys::profile_system(&w.site, &profile_id)
    })
    .await;

    let err = call::<profile::Delete>(&w.caller, &(), &body(json!({ "id": profile_id })))
        .await
        .expect_err("second delete must fail");
    assert_eq!(refusal(&err).code, ErrorCode::ProfileNotFound);
}

/// The read projections behind the GUI's inheritance editor: `defaults`
/// is the stock mapping, `effective` merges a TV's assigned profile over
/// it field by field.
#[tokio::test(flavor = "multi_thread")]
async fn defaults_and_effective_expose_field_resolution() {
    let w = wire().await;
    // The hard-coded floor as a full body — the editable default
    // profile is seeded from this at first boot.
    let stock_map = json!({
        "name": "default",
        "on_startup": { "action": "power_on", "restore_input": true, "while_streaming": false },
        // Floor: off-going events fire while armed (the power-offs plus
        // the DPMS-off/idle-timeout blanks); wake-ish events stay gated.
        "on_shutdown": { "action": "power_off", "restore_input": false, "while_streaming": true },
        "on_reboot": { "action": "none", "restore_input": false, "while_streaming": true },
        "on_sleep": { "action": "power_off", "restore_input": false, "while_streaming": true },
        "on_wake": { "action": "power_on", "restore_input": true, "while_streaming": false },
        "on_dpms_off": { "action": "screen_off", "restore_input": false, "while_streaming": true },
        "on_dpms_on": "screen_on",
        "on_idle_timeout": { "action": "screen_off", "restore_input": false, "while_streaming": true },
        "on_idle_resume": "screen_on",
        "idle_timeout_seconds": 0,
        "automate_when": "always",
        // The going-off events sleep the PC only when asked.
        "on_tv_power_off": "none",
        "on_remote_power": "none",
        "remote_keys": false,
        "on_tv_power_on": "none",
    });

    let defaults = call::<profile::Defaults>(&w.caller, &(), &())
        .await
        .expect("defaults ok");
    assert_eq!(serde_json::to_value(defaults).unwrap(), stock_map);

    // A TV with no profile resolves to the pure defaults.
    let tv = add_tv(&w, "Den", "mock://effective").await;
    let effective = call::<profile::Effective>(&w.caller, &(), &body(json!({ "id": tv })))
        .await
        .expect("effective ok");
    assert_eq!(serde_json::to_value(effective).unwrap(), stock_map);

    let ok = call::<profile::Create>(
        &w.caller,
        &(),
        &body(json!({ "body": {
            "name": "Quiet",
            "on_dpms_off": "none",
            "on_sleep": "screen_off",
            "idle_timeout_seconds": 300,
        } })),
    )
    .await
    .expect("create ok");
    let profile_id = ok.id;
    call::<tv::SetSystemProfile>(&w.caller, &tv, &body(json!({ "profile": profile_id })))
        .await
        .expect("assign ok");

    let effective = call::<profile::Effective>(&w.caller, &(), &body(json!({ "id": tv })))
        .await
        .expect("effective ok");
    assert_eq!(
        serde_json::to_value(effective).unwrap(),
        json!({
            "name": "Quiet",
            "on_startup": { "action": "power_on", "restore_input": true, "while_streaming": false },
            "on_shutdown": { "action": "power_off", "restore_input": false, "while_streaming": true },
            "on_reboot": { "action": "none", "restore_input": false, "while_streaming": true },
            // Locally set on this profile — a bare token replaces the
            // floor's exempted `on_sleep` table wholesale.
            "on_sleep": "screen_off",
            "on_wake": { "action": "power_on", "restore_input": true, "while_streaming": false },
            "on_dpms_off": "none",
            "on_dpms_on": "screen_on",
            // Inherited from the floor: the off-going idle-timeout blank
            // is ungated (fires while armed).
            "on_idle_timeout": { "action": "screen_off", "restore_input": false, "while_streaming": true },
            "on_idle_resume": "screen_on",
            "idle_timeout_seconds": 300,
            "automate_when": "always",
            "on_tv_power_off": "none",
            "on_remote_power": "none",
            "remote_keys": false,
            "on_tv_power_on": "none",
        })
    );

    let err = call::<profile::Effective>(&w.caller, &(), &body(json!({ "id": Uuid::new_v4() })))
        .await
        .expect_err("unknown TV must fail");
    assert_eq!(refusal(&err).code, ErrorCode::TvNotFound);
}

/// Per-TV resolution: an assigned profile's explicit `none` suppresses the
/// stock action, an explicit different action replaces it, and fields the
/// profile leaves unset fall back to the stock mapping.
#[tokio::test(flavor = "multi_thread")]
async fn assigned_profile_overrides_and_falls_back_per_field() {
    let w = wire().await;
    let custom = add_tv(&w, "Custom", "mock://prof-a").await;
    let stock = add_tv(&w, "Stock", "mock://prof-b").await;
    let sub = history_sub(&w.client, keys::tv_all(&w.site)).await;
    wait_all_tv_state(&sub, &w.site, [custom, stock], "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // The profile suppresses the stock DPMS-off blank and inverts DPMS-on
    // into a blank — perverse on purpose, so every override outcome is
    // observable while both mocks stay connected (screen actions never
    // drop the mock's link; power-off would).
    let ok = call::<profile::Create>(&w.caller, &(), &body(json!({ "body": { "name": "Cinema", "on_dpms_off": "none", "on_dpms_on": "screen_off" } })))
    .await
    .expect("create ok");
    let profile_id = ok.id;
    call::<tv::SetSystemProfile>(&w.caller, &custom, &body(json!({ "profile": profile_id })))
        .await
        .expect("assign ok");
    wait_tv_state(&sub, &w.site, custom, "meta/config", |v| {
        v["system_profile"] == profile_id.to_string()
    })
    .await;

    // DPMS off: stock TV blanks, the profile's explicit `none` keeps the
    // custom TV untouched.
    w.stack.world.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: None,
    });
    wait_tv_state(&sub, &w.site, stock, "state/power", |v| {
        v["state"] == "screenoff"
    })
    .await;
    assert_no_sample(&sub, Duration::from_millis(400), |s| {
        keys::parse_tv_path(&w.site, s.key_expr().as_str())
            .is_some_and(|(tv, p)| tv == custom && p == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    // DPMS on: the stock TV wakes its panel, the profile's screen_off
    // override blanks the custom TV instead.
    w.stack.world.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
    });
    let (mut custom_blanked, mut stock_awake) = (false, false);
    recv_until(&sub, |s| {
        if let Some((tv, p)) = keys::parse_tv_path(&w.site, s.key_expr().as_str())
            && p == "state/power"
        {
            let state = payload_json(s)["state"].clone();
            custom_blanked |= tv == custom && state == "screenoff";
            stock_awake |= tv == stock && state == "active";
        }
        custom_blanked && stock_awake
    })
    .await;

    // Shutdown: the profile has no on_shutdown, so the stock power-off
    // applies to the custom TV too.
    w.stack.world.send(InEvent::System {
        event: SystemEvent::Shutdown,
        connector: None,
    });
    wait_all_tv_state(&sub, &w.site, [custom, stock], "state/power", |v| {
        v["state"] == "standby"
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_session_streams_findings_until_stopped() {
    let w = wire().await;

    let err = call::<manager::DiscoverStart>(&w.caller, &(), &body(json!({ "vendor": "nope" })))
        .await
        .expect_err("unknown vendor must fail");
    assert_eq!(refusal(&err).code, ErrorCode::UnknownVendor);

    let ok = call::<manager::DiscoverStart>(&w.caller, &(), &body(json!({ "vendor": "mock" })))
        .await
        .expect("start ok");
    let session = ok.session;

    // The session announces itself via a liveliness token.
    let alive_key = keys::alive_discovery(&w.site, &session);
    wait_liveliness(&w.client, &alive_key, true).await;

    // Findings are plain puts on the session's found/* keys, repeated
    // every scan round — a subscriber sees new TVs as they appear.
    let sub = w
        .client
        .declare_subscriber(format!("tisvlay/{}/discovery/{}/found/*", w.site, session))
        .await
        .unwrap();
    w.handle
        .publish_tv(DiscoveredTv::new("tv-d", "Delta", "mock://d", "mock"));
    // CEC-shaped finding id: a device path. The publish key must
    // sanitize it into one chunk — the raw path would be an invalid
    // key expression and the finding would silently never stream.
    w.handle
        .publish_tv(DiscoveredTv::new("/dev/cec9", "Cec", "/dev/cec9", "mock"));
    let (mut delta_seen, mut cec_seen) = (false, false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(delta_seen && cec_seen) {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .expect("timed out waiting for findings")
            .expect("subscriber closed");
        let finding = payload_json(&sample);
        if finding["name"] == "Delta" {
            assert!(sample.key_expr().as_str().ends_with("/found/tv-d"));
            delta_seen = true;
        }
        if finding["name"] == "Cec" {
            assert!(sample.key_expr().as_str().ends_with("/found/_dev_cec9"));
            assert_eq!(finding["id"], "/dev/cec9", "payload keeps the raw id");
            cec_seen = true;
        }
    }

    call::<manager::DiscoverStop>(&w.caller, &(), &body(json!({ "session": session })))
        .await
        .expect("stop ok");
    wait_liveliness(&w.client, &alive_key, false).await;

    let err = call::<manager::DiscoverStop>(&w.caller, &(), &body(json!({ "session": session })))
        .await
        .expect_err("second stop must fail");
    assert_eq!(refusal(&err).code, ErrorCode::DiscoveryNotFound);
}

/// Poll the liveliness state of `key` until it matches `want`.
async fn wait_liveliness(client: &Session, key: &str, want: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replies = client
            .liveliness()
            .get(key.to_string())
            .timeout(Duration::from_secs(2))
            .await
            .unwrap();
        let mut alive = false;
        while let Ok(reply) = replies.recv_async().await {
            if reply.result().is_ok_and(|s| s.kind() == SampleKind::Put) {
                alive = true;
            }
        }
        if alive == want {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "liveliness of {key} never became {want}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_catalog_and_schema_queryables() {
    let w = wire().await;

    let list = call::<vendors::Catalog>(&w.caller, &(), &())
        .await
        .expect("vendors ok");
    let mock = list
        .iter()
        .find(|v| v.id == "mock")
        .expect("mock vendor listed");
    assert_eq!(mock.name, "Mock");
    assert_eq!(mock.capabilities, ["power", "volume"]);

    let schema = call::<vendors::Schema>(&w.caller, "mock", &())
        .await
        .expect("schema ok");
    let address = schema
        .iter()
        .find(|f| f.name == "address")
        .expect("address field present");
    assert!(address.required);

    let err = call::<vendors::Schema>(&w.caller, "nope", &())
        .await
        .expect_err("unknown vendor must fail");
    assert_eq!(refusal(&err).code, ErrorCode::UnknownVendor);
}

#[tokio::test(flavor = "multi_thread")]
async fn store_round_trips_profiles_and_assignment() {
    let dir = tisvlay_test::socket_tempdir();
    let store = dir.path().join("daemon.toml");
    let socket1: PathBuf = dir.path().join("a.sock");
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
    let ok = call::<profile::Create>(
        &caller1,
        &(),
        &body(json!({ "body": { "name": "Kept", "on_dpms_off": "none" } })),
    )
    .await
    .expect("create ok");
    let profile_id = ok.id;
    let request = rpc::AddTv {
        id: None,
        name: "Den".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": "mock://persist-prof" })).unwrap(),
    };
    let ok = call::<manager::AddTv>(&caller1, &(), &request)
        .await
        .expect("add-tv ok");
    let tv = ok.id;
    call::<tv::SetSystemProfile>(&caller1, &tv, &body(json!({ "profile": profile_id })))
        .await
        .expect("assign ok");

    // Wait for the debounced write that captures the assignment. Both ids
    // reach the file earlier (a new TV skips the debounce), so match the
    // assignment itself.
    let assignment = format!("system_profile = \"{profile_id}\"");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&store).is_ok_and(|t| t.contains(&assignment)) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "store never captured the profile + assignment"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(client1);
    stack1.daemon.close().await.unwrap();

    // Second life: `serve` applies the store before the world runs.
    let socket2 = dir.path().join("b.sock");
    let (connector2, discovery2, _handle2) = new_test_parts();
    let _stack2 = tisvlay_daemon::serve(
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
    let client2 = zenoh::open(bootstrap::client_config(&socket2).unwrap())
        .await
        .unwrap();
    let sub = history_sub(&client2, profile_wild("t")).await;
    recv_until(&sub, |s| {
        s.key_expr().as_str() == keys::profile_system("t", &profile_id)
            && payload_json(s)["name"] == "Kept"
    })
    .await;
    let tv_sub = history_sub(&client2, keys::tv_all("t")).await;
    wait_tv_state(&tv_sub, "t", tv, "meta/config", |v| {
        v["system_profile"] == profile_id.to_string()
    })
    .await;
}
