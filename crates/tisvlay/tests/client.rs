//! The GUI-facing SDK against a live daemon + session service: mirror
//! hydration into `TvInfo`, watcher events, rpcs, and profile bodies.

#![expect(
    clippy::unwrap_used,
    reason = "integration tests fail loudly by design; clippy's allow-*-in-tests keys don't reach helper fns under tests/"
)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tisvlay::{DaemonClient, SessionClient, StateEvent, StateWatcher, StateWatcherConfig};
use tisvlay_mock::new_test_parts;

/// A vendor config object from a JSON literal.
fn config(json: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    serde_json::from_value(json).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn sdk_mirrors_rpcs_and_watches() {
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        &system_socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_capabilities(vec!["power".into()])
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

    let daemon = DaemonClient::connect(&system_socket, "t").await.unwrap();
    let session = SessionClient::connect(&user_socket, "t").await.unwrap();
    let mut watcher =
        StateWatcher::new(&daemon, Some(&session), None, StateWatcherConfig::default());

    // The resilient link dials in the background; wait for liveliness
    // before the first rpc (real clients ride availability the same way).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !daemon.is_available().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon never became available"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Catalog reads work before any TV exists.
    let vendors = daemon.get_supported_vendors().await.unwrap();
    let names: Vec<(&str, &str)> = vendors
        .iter()
        .map(|vendor| (vendor.id.as_str(), vendor.name.as_str()))
        .collect();
    assert_eq!(names, [("mock", "Mock")]);
    let schema = daemon.get_pairing_schema("mock").await.unwrap();
    assert!(schema.iter().any(|field| field.name == "address"));
    assert!(daemon.get_pairing_schema("nope").await.is_err());

    // Pair a TV (mock auto-accepts) and wait for the mirror to see it up.
    let tv = daemon
        .pair_tv(
            "Den",
            "mock",
            config(json!({ "address": "mock://sdk", "pairing": "accept" })),
            true,
        )
        .await
        .unwrap()
        .id;
    wait_for(&daemon, |tvs| {
        tvs.iter()
            .any(|info| info.uuid == tv && info.connected && info.volume == 30)
    })
    .await;

    // The watcher carried TV change events while that happened (other
    // variants — liveliness, session — may interleave first).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let event = tokio::time::timeout_at(deadline, watcher.next())
            .await
            .expect("watcher timed out")
            .expect("watcher closed");
        if matches!(event, StateEvent::DaemonStateChanged(_)) {
            break;
        }
    }

    // Writes round-trip and land back in the mirror.
    daemon.set_volume(tv, 12).await.unwrap();
    wait_for(&daemon, |tvs| {
        tvs.iter().any(|info| info.uuid == tv && info.volume == 12)
    })
    .await;
    let info = daemon
        .list_tvs()
        .await
        .unwrap()
        .into_iter()
        .find(|info| info.uuid == tv)
        .unwrap();
    assert!(
        info.capabilities
            .contains(tisvlay_core::Capabilities::VOLUME)
    );
    assert_eq!(info.power_state, tisvlay_core::PowerState::Active);
    assert!(
        info.vendor_config
            .iter()
            .any(|(label, value)| label == "Address" && value == "mock://sdk"),
        "vendor rows are the connector's labeled display projection, via the mirror: {:?}",
        info.vendor_config
    );
    assert_eq!(
        info.primary_descriptor, "mock://sdk",
        "primary descriptor rides the published identity",
    );

    // System + user profile bodies flow through both clients.
    let profile = daemon.create_system_profile("Cinema").await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if daemon.get_system_profile(profile).await.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "profile never mirrored"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let defaults = daemon.get_resolved_system_profile_defaults().await.unwrap();
    // DPMS-off is an off-going blank: ungated at the floor (fires while
    // armed) since a blank is always safe to let through.
    assert_eq!(
        defaults.on_dpms_off,
        Some(tisvlay_core::EventAction::ungated(
            tisvlay_core::SessionAction::ScreenOff
        ))
    );

    assert!(session.is_available().await);
    let user_profile = session.create_user_profile("Quiet").await.unwrap();
    session
        .set_tv_user_profile(tv, Some(user_profile))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if session.get_tv_user_profile(tv).await == Some(user_profile) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "assignment never mirrored"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let bodies = session.list_user_profile_bodies().await;
    assert_eq!(
        bodies.get(&user_profile).map(|b| b.name.as_str()),
        Some("Quiet")
    );

    // Removal drains the mirror row.
    daemon.remove_tv(tv).await.unwrap();
    wait_for(&daemon, |tvs| !tvs.iter().any(|info| info.uuid == tv)).await;
}

/// The remote-session surface end to end: the manual verb rpc arms a
/// source, the user-socket fact hydrates the session mirror (and the
/// `status` rpc reflects it), and the per-TV armed flag rides the full
/// round-trip through the world back into `TvInfo.remote_armed`.
#[tokio::test(flavor = "multi_thread")]
async fn sdk_remote_session_arms_and_mirrors() {
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

    let daemon = DaemonClient::connect(&system_socket, "t").await.unwrap();
    let session = SessionClient::connect(&user_socket, "t").await.unwrap();
    let mut watcher =
        StateWatcher::new(&daemon, Some(&session), None, StateWatcherConfig::default());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !daemon.is_available().await || !session.is_available().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "peers never became available"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A connected TV the session knows about — the arming set is computed
    // over the session's fleet mirror, so the TV must be up first.
    let tv = daemon
        .pair_tv(
            "Den",
            "mock",
            config(json!({ "address": "mock://remote", "pairing": "accept" })),
            true,
        )
        .await
        .unwrap()
        .id;
    let tv_id = tv;
    wait_for(&daemon, |tvs| {
        tvs.iter().any(|info| info.uuid == tv && info.connected)
    })
    .await;

    // No source yet: the fact is empty (or absent) and nothing is armed.
    assert!(
        session
            .remote_session_status()
            .await
            .unwrap()
            .sources
            .is_empty()
    );

    // Arm a manual source. `manual` always arms, so the connected TV
    // enters the armed set.
    session.remote_session_set("laptop", true).await.unwrap();

    // The user-socket fact hydrates the session mirror: one manual source
    // tagged `laptop`, the TV in the armed list. Drain the watcher until
    // the mirror slot converges (RemoteSessionChanged rides the same
    // stream as the other user deltas).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(state) = session.remote_session().await
            && state
                .sources
                .iter()
                .any(|s| s.kind == tisvlay::RemoteSourceKind::Manual && s.detail == "laptop")
            && state.armed.contains(&tv_id)
        {
            break;
        }
        let event = tokio::time::timeout_at(deadline, watcher.next())
            .await
            .expect("watcher timed out for remote-session")
            .expect("watcher closed");
        let _ = event;
    }
    // The status rpc reports the same live fact.
    let status = session.remote_session_status().await.unwrap();
    assert!(status.armed.contains(&tv_id));

    // The armed flag completes the round-trip through the world and lands
    // on the per-TV meta projection the daemon mirror carries.
    wait_for(&daemon, |tvs| {
        tvs.iter().any(|info| info.uuid == tv && info.remote_armed)
    })
    .await;

    // Disarm: the source clears and the TV disarms both in the fact and
    // in the meta projection.
    session.remote_session_set("laptop", false).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if session
            .remote_session()
            .await
            .is_some_and(|state| state.sources.is_empty() && state.armed.is_empty())
        {
            break;
        }
        let event = tokio::time::timeout_at(deadline, watcher.next())
            .await
            .expect("watcher timed out for disarm")
            .expect("watcher closed");
        let _ = event;
    }
    wait_for(&daemon, |tvs| {
        tvs.iter().any(|info| info.uuid == tv && !info.remote_armed)
    })
    .await;
}

async fn wait_for(daemon: &DaemonClient, pred: impl Fn(&[tisvlay::TvInfo]) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let tvs = daemon.list_tvs().await.unwrap();
        if pred(&tvs) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mirror never converged; last: {tvs:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// No start-order assumptions: a client opened before the daemon's
/// socket even exists must connect once the daemon appears (background
/// retry), flip availability via liveliness, and hydrate the catalog —
/// including the seeded editable default profile.
#[tokio::test(flavor = "multi_thread")]
async fn client_before_daemon_connects_when_it_appears() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("late.sock");

    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    assert!(!daemon.is_available().await, "nothing is listening yet");
    tokio::time::sleep(Duration::from_millis(400)).await;

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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !daemon.is_available().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never noticed the daemon appearing"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Catalog hydration through the late-established link: the seeded
    // default profile is visible and editable-shaped.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(body) = daemon
            .get_system_profile(tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID)
            .await
        {
            assert_eq!(body.name, "default");
            assert_eq!(
                body.on_dpms_off,
                Some(tisvlay_core::EventAction::ungated(
                    tisvlay_core::SessionAction::ScreenOff
                ))
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "default profile never hydrated"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The startup-race regression the querier-matching rework closes: an
/// rpc fired immediately after `connect` — before any liveliness wait,
/// while queryable routes may still be propagating to the fresh peer —
/// succeeds within the matching deadline instead of failing with an
/// instant no-reply. (Asserting eventual success only: routes may also
/// legitimately land before the first attempt.)
#[tokio::test(flavor = "multi_thread")]
async fn first_rpc_races_route_propagation_and_still_lands() {
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        &system_socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_capabilities(vec!["power".into()])
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();

    let daemon = DaemonClient::connect(&system_socket, "t").await.unwrap();
    // No liveliness wait — this is the whole point.
    let vendors = daemon.get_supported_vendors().await.unwrap();
    let names: Vec<(&str, &str)> = vendors
        .iter()
        .map(|vendor| (vendor.id.as_str(), vendor.name.as_str()))
        .collect();
    assert_eq!(names, [("mock", "Mock")]);

    // Typed service errors stay terminal and fast — the matching gate
    // must not swallow them into retries.
    let error = daemon.get_pairing_schema("nope").await.unwrap_err();
    assert_eq!(error.rpc_code(), Some(tisvlay::ErrorCode::UnknownVendor));
}

/// A stand-in service built from the protocol's `serve` dispatch, as a
/// third party might mock one: it listens on `socket`, holds `alive`'s
/// liveliness token, answers `info` with `release`, and reports nothing
/// changed to a reload.
macro_rules! stub_service {
    ($socket:expr, $wild:expr, $alive:expr, $family:ident, $release:expr) => {{
        use tisvlay_protocol::endpoint::$family;
        let session = zenoh::open(tisvlay_protocol::bootstrap::peer_config($socket).unwrap())
            .await
            .unwrap();
        let queryable = session.declare_queryable($wild).await.unwrap();
        let token = session.liveliness().declare_token($alive).await.unwrap();
        let release: String = $release.into();
        let task = tokio::spawn(async move {
            while let Ok(query) = queryable.recv_async().await {
                match $family::Incoming::receive("t", query).await {
                    Some((_, $family::Incoming::Info((), responder))) => {
                        let info = tisvlay_protocol::rpc::ServiceInfo {
                            version: release.clone(),
                            state_path: String::new(),
                        };
                        responder.ok(&info).await;
                    }
                    Some((_, $family::Incoming::Reload((), responder))) => {
                        let report = tisvlay_protocol::rpc::ReloadReport { changed: false };
                        responder.ok(&report).await;
                    }
                    _ => {}
                }
            }
        });
        (session, token, task)
    }};
}

async fn next_matching(
    watcher: &mut StateWatcher,
    want: impl Fn(&StateEvent) -> bool,
) -> StateEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = watcher.next().await.expect("watcher open");
            if want(&event) {
                return event;
            }
        }
    })
    .await
    .expect("the event never arrived")
}

/// A daemon on another release line is refused, gated reads included,
/// until it restarts on a matching release.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_on_another_release_is_refused_until_it_restarts() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("sys.sock");
    let stub = stub_service!(
        &socket,
        tisvlay_protocol::keys::manager_rpc_wild("t"),
        tisvlay_protocol::keys::daemon_alive("t"),
        manager,
        "99.0.0"
    );
    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    let mut watcher = StateWatcher::new(&daemon, None, None, StateWatcherConfig::default());

    // The check runs as soon as the daemon appears, so the watcher hears
    // of the mismatch without a call.
    let event = next_matching(&mut watcher, |e| {
        matches!(e, StateEvent::VersionMismatch { .. })
    })
    .await;
    let StateEvent::VersionMismatch {
        service,
        ours,
        theirs,
    } = event
    else {
        unreachable!()
    };
    assert_eq!(service, tisvlay::Service::Daemon);
    assert_eq!(ours, tisvlay_protocol::VERSION);
    assert_eq!(theirs, "99.0.0");
    assert!(matches!(
        daemon.reload_config().await,
        Err(tisvlay::Error::VersionMismatch { .. })
    ));
    assert!(matches!(
        daemon.list_tvs().await,
        Err(tisvlay::Error::VersionMismatch { .. })
    ));
    assert_eq!(
        daemon.version().await.unwrap(),
        "99.0.0",
        "version() is ungated"
    );

    // The service restarts on this release: its next lifetime is checked
    // afresh, and calls go through.
    let (session, token, task) = stub;
    drop(token);
    task.abort();
    session.close().await.unwrap();
    next_matching(&mut watcher, |e| matches!(e, StateEvent::DaemonDisappeared)).await;
    let _restarted = stub_service!(
        &socket,
        tisvlay_protocol::keys::manager_rpc_wild("t"),
        tisvlay_protocol::keys::daemon_alive("t"),
        manager,
        tisvlay_protocol::VERSION
    );
    next_matching(&mut watcher, |e| matches!(e, StateEvent::DaemonAppeared)).await;
    assert!(!daemon.reload_config().await.unwrap().changed);
    assert!(daemon.list_tvs().await.is_ok());
}

/// The user-socket clients refuse a mismatched service the same way.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_and_tray_clients_refuse_another_release() {
    let dir = tisvlay_test::socket_tempdir();
    let session_socket = dir.path().join("session.sock");
    let tray_socket = dir.path().join("tray.sock");
    let _session_stub = stub_service!(
        &session_socket,
        tisvlay_protocol::keys::user_profile_rpc_wild("t"),
        tisvlay_protocol::keys::user_alive_session("t"),
        user_profile,
        "99.0.0"
    );
    let _tray_stub = stub_service!(
        &tray_socket,
        tisvlay_protocol::keys::user_tray_rpc_wild("t"),
        tisvlay_protocol::keys::user_alive_tray("t"),
        user_tray,
        "99.0.0"
    );
    let session = SessionClient::connect(&session_socket, "t").await.unwrap();
    let tray = tisvlay::TrayClient::connect(&tray_socket, "t")
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(session.is_available().await && tray.is_available().await) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the stubs never appeared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(matches!(
        session.reload_config().await,
        Err(tisvlay::Error::VersionMismatch {
            service: tisvlay::Service::Session,
            ..
        })
    ));
    assert_eq!(
        session.version().await.unwrap(),
        "99.0.0",
        "version() is ungated"
    );
    assert!(matches!(
        tray.reload_config().await,
        Err(tisvlay::Error::VersionMismatch {
            service: tisvlay::Service::Tray,
            ..
        })
    ));
}
