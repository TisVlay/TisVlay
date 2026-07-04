//! The tray settings service end to end: SDK TrayClient ↔ headless
//! `serve_settings` through the session service's user-socket peer.

use std::time::Duration;

use tvnext_client::{NotificationSettingsBody, TrayClient};
use tvnext_tray::notify::{Notifier, load_settings, serve_settings};

#[tokio::test(flavor = "multi_thread")]
async fn settings_round_trip_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let config_path = dir.path().join("tray.toml");
    // The session service owns the user-socket listener the tray's
    // client link and the SDK both route through.
    let (connector, discovery, _handle) = tisvlay_mock::new_test_parts();
    let _daemon = tvnext_daemon::serve(
        &system_socket,
        "t",
        vec![
            tvnext_daemon::VendorReg::new("mock", "Mock", std::sync::Arc::new(connector))
                .with_discovery(std::sync::Arc::new(discovery)),
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

    // Headless stand-in for the tray process: its resilient user link +
    // the settings service (no ksni needed).
    let tray_link = zenoh::open(tvnext_wire::bootstrap::client_config(&user_socket).unwrap())
        .await
        .unwrap();
    let notifier = Notifier::new(load_settings(&config_path).unwrap());
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

    // Test toast rpc acks (display is best-effort headlessly) and the
    // info rpc reports the config location.
    client.send_test_notification().await.unwrap();
    assert_eq!(
        client.config_path().await.unwrap(),
        config_path.display().to_string()
    );
}
