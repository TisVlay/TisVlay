//! The tray's state mirror against the real wire: daemon + session
//! service on one side, `Mirror::apply_*` fed by plain subscribers on
//! the other. No ksni — the SNI bus needs a desktop; the mirror is the
//! part with logic.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{Unary, manager, profile, tv, user_profile, user_stream};
use tisvlay_protocol::{bootstrap, keys, rpc};
use tisvlay_tray::state::Mirror;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::Sample;
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

/// One typed rpc that must succeed.
async fn call<R: Unary>(caller: &Caller, target: &R::Target, request: &R::Request) -> R::Response {
    caller.call::<R>(target, request).await.expect("rpc ok")
}

/// A request from a JSON literal, decoded into the rpc's request type.
fn body<T: serde::de::DeserializeOwned>(json: Value) -> T {
    serde_json::from_value(json).expect("the literal fits the request type")
}

/// Pump subscriber samples into the mirror until `pred` holds.
async fn converge(
    mirror: &mut Mirror,
    site: &str,
    system_sub: &Sub,
    user_sub: &Sub,
    mut pred: impl FnMut(&Mirror) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !pred(mirror) {
        tokio::select! {
            sample = system_sub.recv_async() => {
                mirror.apply_system_sample(site, &sample.expect("system sub closed"));
            }
            sample = user_sub.recv_async() => {
                mirror.apply_user_sample(site, &sample.expect("user sub closed"));
            }
            () = tokio::time::sleep_until(deadline) => {
                panic!("mirror never converged; state: {mirror:#?}");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mirror_converges_from_live_and_hydrated_samples() {
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        &system_socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    let _session = tisvlay_session::serve(tisvlay_session::SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: "t".into(),
        store: None,
        sunshine: tisvlay_session::sunshine::StaticProbe::absent(),
    })
    .await
    .unwrap();

    let system = zenoh::open(bootstrap::client_config(&system_socket).unwrap())
        .await
        .unwrap();
    let user = zenoh::open(bootstrap::client_config(&user_socket).unwrap())
        .await
        .unwrap();
    let system_caller = Caller::new(system.clone(), "t");
    let user_caller = Caller::new(user.clone(), "t");

    // Seed: one TV, one system profile (assigned), one user profile
    // (assigned) — all before the mirror subscribes, so hydration is
    // what delivers them.
    let request = rpc::AddTv {
        id: None,
        name: "Den".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": "mock://tray" })).unwrap(),
    };
    let tv = call::<manager::AddTv>(&system_caller, &(), &request)
        .await
        .id;
    let request = body(json!({ "body": { "name": "Cinema" } }));
    let system_profile = call::<profile::Create>(&system_caller, &(), &request)
        .await
        .id;
    let request = rpc::SetSystemProfile {
        profile: Some(system_profile),
    };
    call::<tv::SetSystemProfile>(&system_caller, &tv, &request).await;
    let request = body(json!({ "body": { "name": "Quiet nights" } }));
    let user_profile = call::<user_profile::Create>(&user_caller, &(), &request)
        .await
        .id;
    let request = rpc::UserAssign {
        tv,
        profile: Some(user_profile),
    };
    call::<user_profile::Assign>(&user_caller, &(), &request).await;

    let system_sub = system
        .declare_subscriber(keys::all("t"))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();
    let user_sub = user
        .declare_subscriber(keys::user_all("t"))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();

    let mut mirror = Mirror::default();
    converge(&mut mirror, "t", &system_sub, &user_sub, |m| {
        m.tvs.get(&tv).is_some_and(|view| {
            view.connected
                && view.is_on
                && view.name == "Den"
                && view.has_volume
                && view.has_mute
                && view.has_screen
                && view.has_backlight
                && view.managed
                && view.automated
                && view.volume == Some(30)
                && !view.inputs.is_empty()
                && view.system_profile == Some(system_profile)
                && view.user_profile == Some(user_profile)
        }) && m.system_profiles.get(&system_profile) == Some(&"Cinema".to_string())
            && m.user_profiles.get(&user_profile) == Some(&"Quiet nights".to_string())
    })
    .await;

    // Live edits flow through: rename the system profile, unassign the
    // user profile, drop the volume.
    let request = body(json!({ "id": system_profile, "body": { "name": "Movie night" } }));
    call::<profile::Update>(&system_caller, &(), &request).await;
    let request = rpc::UserAssign { tv, profile: None };
    call::<user_profile::Assign>(&user_caller, &(), &request).await;
    call::<tv::SetVolume>(&system_caller, &tv, &rpc::SetVolume { level: 11 }).await;
    converge(&mut mirror, "t", &system_sub, &user_sub, |m| {
        m.system_profiles.get(&system_profile) == Some(&"Movie night".to_string())
            && m.tvs
                .get(&tv)
                .is_some_and(|view| view.user_profile.is_none() && view.volume == Some(11))
    })
    .await;

    // A manual stream source arms the TV; the flag rides the meta
    // projection round-trip (session mirror -> world -> daemon cache) and
    // drives the tray's read-only "Stream active" indicator.
    let stream = |active| rpc::StreamSet {
        source: Some("laptop".into()),
        active,
    };
    call::<user_stream::Set>(&user_caller, &(), &stream(true)).await;
    converge(&mut mirror, "t", &system_sub, &user_sub, |m| {
        m.tvs.get(&tv).is_some_and(|view| view.stream_armed)
    })
    .await;
    call::<user_stream::Set>(&user_caller, &(), &stream(false)).await;
    converge(&mut mirror, "t", &system_sub, &user_sub, |m| {
        m.tvs.get(&tv).is_some_and(|view| !view.stream_armed)
    })
    .await;

    // Removal tombstones every key; the meta delete drops the row.
    call::<tv::Remove>(&system_caller, &tv, &()).await;
    converge(&mut mirror, "t", &system_sub, &user_sub, |m| {
        !m.tvs.contains_key(&tv)
    })
    .await;
}

/// Capability flags derive from the identity payload's list, which only
/// fills at connect — a TV whose dial fails (never connected) shows no
/// capability sections and no automation, exactly like a stripped-down
/// CEC backend would post-connect.
#[tokio::test(flavor = "multi_thread")]
async fn capability_flags_gate_menu_sections() {
    let dir = tisvlay_test::socket_tempdir();
    let socket: &Path = &dir.path().join("c.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    let client = zenoh::open(bootstrap::client_config(socket).unwrap())
        .await
        .unwrap();
    let request = rpc::AddTv {
        id: None,
        name: "Caps".into(),
        vendor: "mock".into(),
        managed: false,
        config: serde_json::from_value(json!({ "address": "mock://caps", "refuse_connect": true }))
            .unwrap(),
    };
    let tv = call::<manager::AddTv>(&Caller::new(client.clone(), "t"), &(), &request)
        .await
        .id;

    let sub = client
        .declare_subscriber(keys::tv_all("t"))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();
    let mut mirror = Mirror::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !mirror
        .tvs
        .get(&tv)
        .is_some_and(|view| view.name == "Caps" && !view.managed && !view.automated)
    {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .expect("timed out")
            .expect("sub closed");
        mirror.apply_system_sample("t", &sample);
    }
    let view = &mirror.tvs[&tv];
    assert!(
        !view.has_volume && !view.has_mute && !view.has_screen && !view.has_backlight,
        "capability sections stay hidden until a connection reports them"
    );
    assert!(!view.connected);
}
