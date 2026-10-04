//! The tray settings service end to end: SDK TrayClient ↔ headless
//! `serve_settings` through the session service's user-socket peer.

use std::time::Duration;

use tisvlay::{NotificationSettingsBody, TrayClient};
use tisvlay_tray::notify::{Notifier, load_settings, serve_settings};

#[tokio::test(flavor = "multi_thread")]
async fn settings_round_trip_and_persistence() {
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let config_path = dir.path().join("tray.toml");
    // The session service owns the user-socket listener the tray's
    // client link and the SDK both route through.
    let (connector, discovery, _handle) = tisvlay_mock::new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        &system_socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", std::sync::Arc::new(connector))
                .with_discovery(std::sync::Arc::new(discovery)),
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
        sunshine: tisvlay_session::sunshine::StaticProbe::new(
            tisvlay_session::sunshine::Observation::Absent,
        ),
    })
    .await
    .unwrap();

    // Headless stand-in for the tray process: its resilient user link +
    // the settings service (no ksni needed).
    let tray_link = zenoh::open(tisvlay_wire::bootstrap::client_config(&user_socket).unwrap())
        .await
        .unwrap();
    let (notifier, mut toasts) = Notifier::capturing(load_settings(&config_path).unwrap());
    let service = tokio::spawn(serve_settings(
        tray_link,
        "t".into(),
        config_path.clone(),
        notifier,
    ));

    let client = TrayClient::connect(&user_socket, "t").await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !client.is_available().await {
        assert!(
            !service.is_finished(),
            "settings service died: {:?}",
            service.await
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "tray liveliness never appeared"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let settings = client.notification_settings().await.unwrap();
    assert_eq!(settings.tv_wake, Some(true), "factory default on");

    // Partial update: only the touched switch changes, and it persists.
    client
        .update_notification_settings(NotificationSettingsBody {
            tv_wake: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    let settings = client.notification_settings().await.unwrap();
    assert_eq!(settings.tv_wake, Some(false));
    assert_eq!(settings.tv_pairing, Some(true));
    let on_disk = load_settings(&config_path).unwrap();
    assert!(!on_disk.tv_wake && on_disk.tv_pairing);

    // The test toast rpc acks and queues the toast (bypassing the category
    // gate); the info rpc reports the config location.
    client.send_test_notification().await.unwrap();
    let toast = tokio::time::timeout(std::time::Duration::from_secs(5), toasts.recv())
        .await
        .expect("test toast queued")
        .expect("notifier alive");
    assert_eq!(toast.summary, "Test notification");
    assert_eq!(
        client.config_path().await.unwrap(),
        config_path.display().to_string()
    );
}

/// The GUI's start-order QA scenario: SDK clients exist (and their
/// liveliness subscribers are declared) BEFORE the user-socket router
/// or any service exists. When the stack comes up afterward, both
/// availabilities must flip without recreating the clients.
#[tokio::test(flavor = "multi_thread")]
async fn clients_before_stack_see_liveliness_when_it_appears() {
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let config_path = dir.path().join("tray.toml");

    // Clients first — nothing is listening on either socket yet.
    let session_client = tisvlay::SessionClient::connect(&user_socket, "t")
        .await
        .unwrap();
    let tray_client = TrayClient::connect(&user_socket, "t").await.unwrap();
    assert!(!session_client.is_available().await);
    assert!(!tray_client.is_available().await);
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Now the stack: daemon, session service (router hub), tray
    // settings stand-in.
    let (connector, discovery, _handle) = tisvlay_mock::new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        &system_socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", std::sync::Arc::new(connector))
                .with_discovery(std::sync::Arc::new(discovery)),
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
        sunshine: tisvlay_session::sunshine::StaticProbe::new(
            tisvlay_session::sunshine::Observation::Absent,
        ),
    })
    .await
    .unwrap();
    let tray_link = zenoh::open(tisvlay_wire::bootstrap::client_config(&user_socket).unwrap())
        .await
        .unwrap();
    let (notifier, _toasts) = Notifier::capturing(load_settings(&config_path).unwrap());
    let service = tokio::spawn(serve_settings(
        tray_link,
        "t".into(),
        config_path.clone(),
        notifier,
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !(session_client.is_available().await && tray_client.is_available().await) {
        assert!(
            !service.is_finished(),
            "settings service died: {:?}",
            service.await
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "liveliness never reached the pre-existing clients: session={} tray={}",
            session_client.is_available().await,
            tray_client.is_available().await,
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A watcher constructed only NOW — after every liveliness edge
    // already fired into the clients' broadcasts — must still deliver
    // the Appeared events (the watcher synthesizes them from mirror
    // state after subscribing). This is the GUI's footer-indicator
    // path: edges lost to a receiver-less broadcast must not leave
    // event-driven consumers permanently stale.
    let daemon_client = tisvlay::DaemonClient::connect(&system_socket, "t")
        .await
        .unwrap();
    let mut watcher = tisvlay::StateWatcher::new(
        &daemon_client,
        Some(&session_client),
        Some(&tray_client),
        tisvlay::StateWatcherConfig::default(),
    );
    let (mut session_appeared, mut tray_appeared) = (false, false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(session_appeared && tray_appeared) {
        let event = tokio::time::timeout_at(deadline, watcher.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "late watcher missed Appeared events: session={session_appeared} tray={tray_appeared}"
                )
            })
            .expect("watcher closed");
        match event {
            tisvlay::StateEvent::SessionAppeared => session_appeared = true,
            tisvlay::StateEvent::TrayAppeared => tray_appeared = true,
            _ => {}
        }
    }
}
