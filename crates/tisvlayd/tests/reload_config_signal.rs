//! Regression: `Manager1.ConfigReloaded` must fire on **both** the
//! success and failure paths of `ReloadConfig`. The GUI relies on the
//! failure signal to surface "config reload failed" toasts when the
//! daemon was reloaded by something it didn't drive (SIGHUP, another
//! client). Pre-extension, the signal was zero-arg and only fired on
//! success — a failed reload was silent to every client except the
//! caller that owned the RPC.

#![cfg(feature = "mock")]

use std::time::Duration;

use tisvlay_core::DaemonClient;
use tisvlay_harness::daemon::TestDaemon;
use tisvlay_test::PrivateBus;
use zbus::MatchRule;

async fn client() -> DaemonClient {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    DaemonClient::from_connection(conn)
        .await
        .expect("daemon client")
}

async fn subscribe_config_reloaded() -> zbus::MessageStream {
    let conn = PrivateBus::shared().connect().await.expect("bus conn");
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.tisvlay.Manager1")
        .expect("iface")
        .member("ConfigReloaded")
        .expect("member")
        .build();
    zbus::MessageStream::for_match_rule(rule, &conn, None)
        .await
        .expect("match rule")
}

async fn expect_signal(stream: &mut zbus::MessageStream) -> (bool, String) {
    use futures_util::StreamExt;
    let msg = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("ConfigReloaded did not arrive within 2s")
        .expect("stream ended")
        .expect("message");
    msg.body().deserialize().expect("deserialize signal body")
}

#[tokio::test]
async fn reload_config_emits_success_signal_on_clean_reload() {
    let _daemon = TestDaemon::with_one_mock_tv("reload_ok")
        .await
        .expect("daemon");
    let client = client().await;

    let mut stream = subscribe_config_reloaded().await;
    client.reload_config().await.expect("reload should succeed");

    let (success, error_msg) = expect_signal(&mut stream).await;
    assert!(success, "clean reload should emit success=true");
    assert!(
        error_msg.is_empty(),
        "clean reload should not carry an error message, got: {error_msg}",
    );
}

#[tokio::test]
async fn reload_config_emits_failure_signal_on_invalid_config() {
    let daemon = TestDaemon::with_one_mock_tv("reload_bad")
        .await
        .expect("daemon");
    let client = client().await;

    // Subscribe before corrupting the file so we can't race the emit.
    let mut stream = subscribe_config_reloaded().await;

    // Overwrite the config with garbage so the load step fails.
    std::fs::write(daemon.config_path(), "this is not valid toml = [\n")
        .expect("write garbage config");

    let err = client
        .reload_config()
        .await
        .expect_err("reload with invalid config must fail");
    let rpc_msg = format!("{err:#}");
    assert!(
        !rpc_msg.is_empty(),
        "RPC error should carry a non-empty message",
    );

    let (success, error_msg) = expect_signal(&mut stream).await;
    assert!(!success, "failed reload should emit success=false");
    assert!(
        !error_msg.is_empty(),
        "failed reload should carry a non-empty error message so the GUI can render a toast",
    );
}
