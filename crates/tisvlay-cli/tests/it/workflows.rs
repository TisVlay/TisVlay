use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

struct CliDaemon {
    _directory: tempfile::TempDir,
    _stack: tisvlay_daemon::Stack,
    session: Option<tisvlay_session::SessionStack>,
    socket: PathBuf,
    user_socket: PathBuf,
}

impl CliDaemon {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        let (connector, discovery, _handle) = tisvlay_mock::new_test_parts();
        let stack = tisvlay_daemon::serve(
            &socket,
            "test",
            vec![
                tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                    .with_discovery(Arc::new(discovery)),
            ],
            None,
        )
        .await
        .unwrap();
        let user_socket = directory.path().join("session.sock");
        Self {
            _directory: directory,
            _stack: stack,
            session: None,
            socket,
            user_socket,
        }
    }

    async fn with_session() -> Self {
        let mut rig = Self::start().await;
        rig.session = Some(
            tisvlay_session::serve(tisvlay_session::SessionOptions {
                system_socket: rig.socket.clone(),
                user_socket: rig.user_socket.clone(),
                site: "test".into(),
                store: None,
                sunshine: tisvlay_session::sunshine::StaticProbe::absent(),
            })
            .await
            .unwrap(),
        );
        rig
    }

    async fn command(&self, args: &[&str]) -> Output {
        tokio::time::timeout(
            Duration::from_secs(15),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_tisvlay"))
                .env_remove("COMPLETE")
                .env("TOKIO_WORKER_THREADS", "2")
                .arg("--socket")
                .arg(&self.socket)
                .arg("--user-socket")
                .arg(&self.user_socket)
                .args(["--site", "test"])
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("CLI command timed out")
        .expect("run tisvlay")
    }

    async fn success(&self, args: &[&str]) -> String {
        let output = self.command(args).await;
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    async fn json(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.insert(0, "--json");
        serde_json::from_str(&self.success(&args).await).unwrap()
    }

    async fn failure(&self, args: &[&str], expected: &str) {
        let output = self.command(args).await;
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(expected),
            "{args:?}: expected {expected:?}, got {error:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tv_commands_round_trip_and_render_state() {
    let rig = CliDaemon::start().await;
    assert!(rig.success(&["status"]).await.contains("daemon"));
    assert!(rig.json(&["status"]).await.is_object());
    assert_eq!(rig.json(&["vendors"]).await[0]["id"], "mock");
    let schema = rig.json(&["schema", "mock"]).await;
    assert!(
        schema
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["name"] == "address")
    );
    assert!(rig.success(&["list"]).await.contains("No TVs configured"));
    rig.success(&[
        "pair",
        "Office",
        "--vendor",
        "mock",
        "--params",
        r#"{"address":"mock://office","pairing":"accept"}"#,
    ])
    .await;
    let listed = rig.json(&["list"]).await;
    let id = listed.as_object().unwrap().keys().next().unwrap().clone();
    rig.success(&["managed", &id, "false"]).await;
    rig.success(&["set-volume", &id, "64"]).await;
    rig.success(&["backlight", &id, "24"]).await;
    rig.success(&["mute", &id, "true"]).await;
    rig.success(&["rename", &id, "Renamed office"]).await;
    let state = rig.json(&["list"]).await;
    assert_eq!(state[&id]["meta/identity"]["name"], "Renamed office");
    assert_eq!(state[&id]["state/volume"]["level"], 64);
    assert_eq!(state[&id]["state/volume"]["muted"], true);
    assert_eq!(state[&id]["state/backlight"]["level"], 24);
    let rendered = rig.success(&["list"]).await;
    assert!(rendered.contains("Renamed office"));
    assert!(rendered.contains("mock://office"));
    assert!(rendered.contains("[unmanaged]"));
    rig.success(&["remove", &id]).await;
    assert!(rig.json(&["list"]).await.as_object().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_configuration_and_catalog_commands_round_trip() {
    let rig = CliDaemon::start().await;
    assert!(rig.success(&["vendors"]).await.contains("Mock"));
    assert!(rig.success(&["schema", "mock"]).await.contains("address"));
    assert!(
        rig.success(&["discover", "--vendor", "mock", "--timeout-ms", "10"])
            .await
            .contains("nothing found")
    );
    assert_eq!(
        rig.json(&["discover", "--vendor", "mock", "--timeout-ms", "10"])
            .await,
        serde_json::json!([])
    );
    assert!(rig.json(&["profile", "defaults"]).await.is_object());

    let created = rig.success(&["profile", "create", "Work"]).await;
    let profile = created.trim().strip_prefix("created: ").unwrap();
    uuid::Uuid::parse_str(profile).unwrap();
    rig.success(&["profile", "update", profile, r#"{"name":"Work edited"}"#])
        .await;
    let duplicated = rig
        .success(&["profile", "duplicate", profile, "Work copy"])
        .await;
    let duplicate = duplicated.trim().strip_prefix("created: ").unwrap();
    let catalog = rig.json(&["profile", "list"]).await;
    assert_eq!(catalog[profile]["name"], "Work edited");
    assert_eq!(catalog[duplicate]["name"], "Work copy");
    assert!(
        rig.success(&["profile", "list"])
            .await
            .contains("Work edited")
    );

    let added = rig
        .success(&["add", "Office", "--address", "mock://office", "--unmanaged"])
        .await;
    let tv = added.trim().strip_prefix("added: ").unwrap();
    uuid::Uuid::parse_str(tv).unwrap();
    rig.success(&["assign", tv, profile]).await;
    assert!(rig.json(&["profile", "effective", tv]).await.is_object());
    rig.success(&["assign", tv]).await;
    rig.success(&[
        "config-set",
        tv,
        r#"{"name":"Configured office","pairing":"accept"}"#,
    ])
    .await;
    let config = rig.json(&["config", tv]).await;
    assert!(config.to_string().contains("Configured office"));
    assert!(rig.success(&["config", tv]).await.contains("mock://office"));
    rig.success(&["input", tv, "HDMI_2"]).await;
    rig.success(&["primary-input", tv, "HDMI_1"]).await;
    rig.success(&["primary-input", tv]).await;
    rig.success(&["screen", tv, "off"]).await;
    rig.success(&["screen", tv, "on"]).await;
    rig.success(&["repair", tv]).await;
    rig.success(&["power", tv, "off"]).await;
    rig.success(&["power", tv, "on"]).await;
    rig.success(&["reconnect", tv]).await;
    rig.success(&["profile", "delete", duplicate]).await;
    assert!(
        rig.json(&["profile", "list"])
            .await
            .get(duplicate)
            .is_none()
    );
    rig.success(&["remove", tv]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_input_and_rpc_refusals_exit_unsuccessfully() {
    let rig = CliDaemon::start().await;
    let missing = uuid::Uuid::new_v4().to_string();
    rig.failure(&["set-volume", "not-a-uuid", "20"], "invalid value")
        .await;
    rig.failure(&["config-set", &missing, "[]"], "JSON object")
        .await;
    rig.failure(
        &["profile", "create", "Work", "--body", "[]"],
        "JSON object",
    )
    .await;
    rig.failure(
        &["pair", "Office", "--vendor", "mock", "--params", "[]"],
        "JSON object",
    )
    .await;
    rig.failure(&["schema", "missing-vendor"], "unknown-vendor")
        .await;
    rig.failure(&["remove", &missing], "not-found").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn user_profiles_and_remote_sources_use_the_session_socket() {
    let rig = CliDaemon::with_session().await;
    assert!(rig.json(&["user-profile", "defaults"]).await.is_object());
    let created = rig.success(&["user-profile", "create", "Personal"]).await;
    let profile = created.trim().strip_prefix("created: ").unwrap();
    rig.success(&[
        "user-profile",
        "update",
        profile,
        r#"{"name":"Personal edited"}"#,
    ])
    .await;
    let duplicated = rig
        .success(&["user-profile", "duplicate", profile, "Personal copy"])
        .await;
    let duplicate = duplicated.trim().strip_prefix("created: ").unwrap();
    let catalog = rig.json(&["user-profile", "list"]).await;
    assert_eq!(catalog[profile]["name"], "Personal edited");
    assert_eq!(catalog[duplicate]["name"], "Personal copy");
    assert!(
        rig.success(&["user-profile", "list"])
            .await
            .contains("Personal edited")
    );
    let added = rig
        .success(&["add", "Office", "--address", "mock://office", "--unmanaged"])
        .await;
    let tv = added.trim().strip_prefix("added: ").unwrap();
    rig.success(&["user-profile", "assign", tv, profile]).await;
    assert!(
        rig.json(&["user-profile", "effective", tv])
            .await
            .is_object()
    );
    rig.success(&["user-profile", "assign", tv]).await;
    rig.success(&["user-profile", "delete", duplicate]).await;
    assert!(
        rig.json(&["user-profile", "list"])
            .await
            .get(duplicate)
            .is_none()
    );

    assert!(
        rig.success(&["remote-session", "start", "--source", "cli-test"])
            .await
            .contains("armed source 'cli-test'")
    );
    assert!(
        rig.json(&["remote-session", "status"])
            .await
            .to_string()
            .contains("cli-test")
    );
    assert!(
        rig.success(&["remote-session", "status"])
            .await
            .contains("cli-test")
    );
    assert!(
        rig.success(&["remote-session", "stop", "--source", "cli-test"])
            .await
            .contains("disarmed source 'cli-test'")
    );
    assert!(
        !rig.json(&["remote-session", "status"])
            .await
            .to_string()
            .contains("cli-test")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_reports_service_failures_and_absent_services() {
    let rig = CliDaemon::start().await;
    let output = rig.command(&["--json", "reload"]).await;
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daemon"]["status"], "failed");
    assert_eq!(report["session"]["status"], "not-running");
    assert_eq!(report["tray"]["status"], "not-running");
    let rig = CliDaemon::with_session().await;
    let output = rig.command(&["--json", "reload"]).await;
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daemon"]["status"], "failed");
    assert_eq!(report["session"]["status"], "failed");
    assert_eq!(report["tray"]["status"], "not-running");
    let output = rig.command(&["reload"]).await;
    assert!(!output.status.success());
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(rendered.contains("daemon:"));
    assert!(rendered.contains("session:"));
    assert!(rendered.contains("not running"));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("reload failed")
    );
}
