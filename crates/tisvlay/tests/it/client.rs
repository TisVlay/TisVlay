//! The GUI-facing SDK against a live daemon + session service: mirror
//! hydration into `TvInfo`, watcher events, rpcs, and profile bodies.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tisvlay::{DaemonClient, SessionClient, StateEvent, StateWatcher, StateWatcherConfig};
use tisvlay_mock::new_test_parts;

use crate::fake::{FakeService, Family};

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

/// The stream surface end to end: the manual verb rpc arms a
/// source, the user-socket fact hydrates the session mirror (and the
/// `status` rpc reflects it), and the per-TV armed flag rides the full
/// round-trip through the world back into `TvInfo.stream_armed`.
#[tokio::test(flavor = "multi_thread")]
async fn sdk_stream_arms_and_mirrors() {
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
    assert!(session.stream_status().await.unwrap().sources.is_empty());

    // Arm a manual source. `manual` always arms, so the connected TV
    // enters the armed set.
    session.stream_set("laptop", true).await.unwrap();

    // The user-socket fact hydrates the session mirror: one manual source
    // tagged `laptop`, the TV in the armed list. Drain the watcher until
    // the mirror slot converges (StreamChanged rides the same
    // stream as the other user deltas).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(state) = session.stream_state().await
            && state
                .sources
                .iter()
                .any(|s| s.kind == tisvlay::StreamSourceKind::Manual && s.detail == "laptop")
            && state.armed.contains(&tv_id)
        {
            break;
        }
        let event = tokio::time::timeout_at(deadline, watcher.next())
            .await
            .expect("watcher timed out for stream")
            .expect("watcher closed");
        let _ = event;
    }
    // The status rpc reports the same live fact.
    let status = session.stream_status().await.unwrap();
    assert!(status.armed.contains(&tv_id));

    // The armed flag completes the round-trip through the world and lands
    // on the per-TV meta projection the daemon mirror carries.
    wait_for(&daemon, |tvs| {
        tvs.iter().any(|info| info.uuid == tv && info.stream_armed)
    })
    .await;

    // Disarm: the source clears and the TV disarms both in the fact and
    // in the meta projection.
    session.stream_set("laptop", false).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if session
            .stream_state()
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
        tvs.iter().any(|info| info.uuid == tv && !info.stream_armed)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_restart_discards_previous_lifetime_tvs_and_profiles() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("restart.sock");
    let (connector, discovery, _handle) = new_test_parts();
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
    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    let mut watcher = StateWatcher::new(&daemon, None, None, StateWatcherConfig::default());
    next_matching(&mut watcher, |event| {
        matches!(event, StateEvent::DaemonAppeared)
    })
    .await;
    let old_tv = daemon
        .pair_tv(
            "Old TV",
            "mock",
            config(json!({"address": "mock://old", "pairing": "accept"})),
            false,
        )
        .await
        .unwrap()
        .id;
    wait_for(&daemon, |tvs| {
        tvs.iter().any(|tv| tv.uuid == old_tv && tv.connected)
    })
    .await;
    let old_profile = daemon.create_system_profile("Old profile").await.unwrap();
    next_matching(
        &mut watcher,
        |event| matches!(event, StateEvent::SystemProfileAdded(id) if *id == old_profile),
    )
    .await;
    assert!(
        daemon
            .list_system_profile_bodies()
            .await
            .contains_key(&old_profile)
    );
    stack.run_until(std::future::ready(())).await.unwrap();
    next_matching(&mut watcher, |event| {
        matches!(event, StateEvent::DaemonDisappeared)
    })
    .await;
    assert!(daemon.list_system_profile_bodies().await.is_empty());
    let (connector, discovery, _handle) = new_test_parts();
    let restarted = tisvlay_daemon::serve(
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
    next_matching(&mut watcher, |event| {
        matches!(event, StateEvent::DaemonAppeared)
    })
    .await;
    assert!(daemon.list_tvs().await.unwrap().is_empty());
    assert!(
        !daemon
            .list_system_profile_bodies()
            .await
            .contains_key(&old_profile)
    );
    let new_tv = daemon
        .pair_tv(
            "New TV",
            "mock",
            config(json!({"address": "mock://new", "pairing": "accept"})),
            false,
        )
        .await
        .unwrap()
        .id;
    wait_for(&daemon, |tvs| {
        tvs.len() == 1 && tvs[0].uuid == new_tv && tvs[0].connected
    })
    .await;
    restarted.run_until(std::future::ready(())).await.unwrap();
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
    let stub = FakeService::start(&socket, Family::Daemon, "99.0.0").await;
    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    let mut watcher = StateWatcher::new(&daemon, None, None, StateWatcherConfig::default());

    // The check runs as soon as the daemon appears, so the watcher hears
    // of its release, and of the mismatch, without a call.
    next_matching(&mut watcher, |e| {
        matches!(e, StateEvent::ReleaseChecked(tisvlay::Service::Daemon))
    })
    .await;
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
    assert!(matches!(
        daemon.pairing_progress_stream().await,
        Err(tisvlay::Error::VersionMismatch { .. })
    ));
    assert!(matches!(
        daemon.discovery_found_stream().await,
        Err(tisvlay::Error::VersionMismatch { .. })
    ));
    assert_eq!(
        daemon.version().await.unwrap(),
        "99.0.0",
        "version() is ungated"
    );
    assert_eq!(
        daemon.service_info().await.unwrap().version,
        "99.0.0",
        "service_info() is ungated"
    );

    // The service restarts on this release: its next lifetime is checked
    // afresh, and calls go through.
    stub.stop().await;
    next_matching(&mut watcher, |e| matches!(e, StateEvent::DaemonDisappeared)).await;
    assert!(
        daemon.service_info().await.is_none(),
        "the release clears before the disappearance is announced"
    );
    let _restarted = FakeService::start(&socket, Family::Daemon, tisvlay_protocol::VERSION).await;
    next_matching(&mut watcher, |e| matches!(e, StateEvent::DaemonAppeared)).await;
    next_matching(&mut watcher, |e| {
        matches!(e, StateEvent::ReleaseChecked(tisvlay::Service::Daemon))
    })
    .await;
    assert_eq!(
        daemon.service_info().await.unwrap().version,
        tisvlay_protocol::VERSION
    );
    assert!(!daemon.reload_config().await.unwrap().changed);
    assert!(daemon.list_tvs().await.is_ok());
}

/// A release check that fails is retried while the service stays up, so
/// its release is known without any call: here the daemon's liveliness
/// token comes up before it serves its `info` rpc.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_release_check_is_retried() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("sys.sock");
    let session = zenoh::open(tisvlay_protocol::bootstrap::peer_config(&socket).unwrap())
        .await
        .unwrap();
    let _token = session
        .liveliness()
        .declare_token(tisvlay_protocol::keys::daemon_alive("t"))
        .await
        .unwrap();
    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    let mut watcher = StateWatcher::new(&daemon, None, None, StateWatcherConfig::default());
    next_matching(&mut watcher, |e| matches!(e, StateEvent::DaemonAppeared)).await;
    // Outlast the first check, which waits out the matching deadline.
    tokio::time::sleep(tisvlay_protocol::caller::MATCHING_DEADLINE + Duration::from_millis(500))
        .await;
    assert!(daemon.service_info().await.is_none());
    let queryable = session
        .declare_queryable(tisvlay_protocol::keys::manager_rpc_wild("t"))
        .await
        .unwrap();
    let _serve = tokio::spawn(async move {
        use tisvlay_protocol::endpoint::manager;
        while let Ok(query) = queryable.recv_async().await {
            if let Some(((), manager::Incoming::Info((), responder))) =
                manager::Incoming::receive("t", query).await
            {
                let info = tisvlay::ServiceInfo {
                    version: tisvlay_protocol::VERSION.into(),
                    state_path: "/var/lib/tisvlay/state.toml".into(),
                    sleep: None,
                    keys: Vec::new(),
                };
                responder.ok(&info).await;
            }
        }
    });
    next_matching(&mut watcher, |e| {
        matches!(e, StateEvent::ReleaseChecked(tisvlay::Service::Daemon))
    })
    .await;
    let info = daemon.service_info().await.unwrap();
    assert_eq!(info.version, tisvlay_protocol::VERSION);
    assert_eq!(info.state_path, "/var/lib/tisvlay/state.toml");
}

/// `try_next` hands over a queued event without waiting, and nothing when
/// the queue is empty.
#[tokio::test(flavor = "multi_thread")]
async fn try_next_drains_without_waiting() {
    let dir = tisvlay_test::socket_tempdir();
    let daemon = DaemonClient::connect(&dir.path().join("sys.sock"), "t")
        .await
        .unwrap();
    let mut idle = StateWatcher::new(&daemon, None, None, StateWatcherConfig::default());
    assert!(idle.try_next().is_none(), "no service, no events");
    let mut config = StateWatcherConfig::default();
    config.fallback_poll_interval = Duration::from_millis(20);
    let mut polling = StateWatcher::new(&daemon, None, None, config);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(polling.try_next(), Some(StateEvent::FallbackPoll)));
}

/// A watcher built after the mismatch is already known still hears of it:
/// the announcement went out before it subscribed, so it replays it.
#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_built_late_still_hears_of_a_known_mismatch() {
    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("sys.sock");
    let _stub = FakeService::start(&socket, Family::Daemon, "99.0.0").await;
    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    // A call settles the verdict before any watcher exists.
    assert!(matches!(
        daemon.reload_config().await,
        Err(tisvlay::Error::VersionMismatch { .. })
    ));
    let mut watcher = StateWatcher::new(&daemon, None, None, StateWatcherConfig::default());
    next_matching(&mut watcher, |e| {
        matches!(e, StateEvent::ReleaseChecked(tisvlay::Service::Daemon))
    })
    .await;
    let event = next_matching(&mut watcher, |e| {
        matches!(e, StateEvent::VersionMismatch { .. })
    })
    .await;
    assert!(matches!(
        event,
        StateEvent::VersionMismatch { theirs, .. } if theirs == "99.0.0"
    ));
}

/// The user-socket clients refuse a mismatched service the same way.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_and_tray_clients_refuse_another_release() {
    let dir = tisvlay_test::socket_tempdir();
    let session_socket = dir.path().join("session.sock");
    let tray_socket = dir.path().join("tray.sock");
    let _session_stub = FakeService::start(&session_socket, Family::Session, "99.0.0").await;
    let _tray_stub = FakeService::start(&tray_socket, Family::Tray, "99.0.0").await;
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
    assert_eq!(session.service_info().await.unwrap().version, "99.0.0");
    assert_eq!(tray.service_info().await.unwrap().version, "99.0.0");
    assert!(
        tray.notification_settings().await.is_none(),
        "the settings mirror is gated like the others"
    );
}
