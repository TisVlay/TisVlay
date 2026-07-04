//! The GUI-facing SDK against a live daemon + session service: mirror
//! hydration into `TvInfo`, watcher events, rpcs, and profile bodies.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tisvlay_mock::new_test_parts;
use tvnext_client::{DaemonClient, SessionClient, StateEvent, StateWatcher, StateWatcherConfig};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn sdk_mirrors_rpcs_and_watches() {
    let dir = tempfile::tempdir().unwrap();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let _daemon = tvnext_daemon::serve(
        &system_socket,
        "t",
        vec![
            tvnext_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_capabilities(vec!["power".into()])
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    let _session = tvnext_session::serve(tvnext_session::SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: "t".into(),
        store: None,
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
    assert_eq!(vendors, vec![("mock".to_string(), "Mock".to_string())]);
    let schema = daemon.get_pairing_schema("mock").await.unwrap();
    assert!(schema.iter().any(|field| field.name == "address"));
    assert!(daemon.get_pairing_schema("nope").await.is_err());

    // Pair a TV (mock auto-accepts) and wait for the mirror to see it up.
    let tv = daemon
        .pair_tv(
            "Den",
            "mock",
            &json!({ "address": "mock://sdk", "pairing": "accept" }).to_string(),
            true,
        )
        .await
        .unwrap();
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
    daemon.set_volume(&tv, 12).await.unwrap();
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
    assert_eq!(info.power_state, "active");
    assert!(
        info.vendor_config
            .iter()
            .any(|(key, value)| key == "address" && value == "mock://sdk"),
        "vendor config rows come from get-config: {:?}",
        info.vendor_config
    );

    // System + user profile bodies flow through both clients.
    let profile = daemon.create_system_profile("Cinema").await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if daemon.get_system_profile(profile).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "profile never mirrored"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let defaults = daemon.get_resolved_system_profile_defaults().await.unwrap();
    assert_eq!(
        defaults.on_dpms_off,
        Some(tisvlay_core::SessionAction::ScreenOff)
    );

    assert!(session.is_available().await);
    let user_profile = session.create_user_profile("Quiet").await.unwrap();
    session
        .set_tv_user_profile(&tv, &user_profile.to_string())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if session.get_tv_user_profile(&tv).await.unwrap() == user_profile.to_string() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "assignment never mirrored"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let bodies = session.list_user_profile_bodies().await.unwrap();
    assert_eq!(
        bodies.get(&user_profile).map(|b| b.name.as_str()),
        Some("Quiet")
    );

    // Removal drains the mirror row.
    daemon.remove_tv(&tv).await.unwrap();
    wait_for(&daemon, |tvs| !tvs.iter().any(|info| info.uuid == tv)).await;
    let _ = Uuid::parse_str(&tv).unwrap();
}

async fn wait_for(daemon: &DaemonClient, pred: impl Fn(&[tvnext_client::TvInfo]) -> bool) {
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
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("late.sock");

    let daemon = DaemonClient::connect(&socket, "t").await.unwrap();
    assert!(!daemon.is_available().await, "nothing is listening yet");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let (connector, discovery, _handle) = new_test_parts();
    let _stack = tvnext_daemon::serve(
        &socket,
        "t",
        vec![
            tvnext_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
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
        if let Ok(body) = daemon
            .get_system_profile(tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID)
            .await
        {
            assert_eq!(body.name, "default");
            assert_eq!(
                body.on_dpms_off,
                Some(tisvlay_core::SessionAction::ScreenOff)
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
