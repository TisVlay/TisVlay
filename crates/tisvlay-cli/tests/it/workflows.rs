#![expect(
    unsafe_code,
    reason = "sending SIGINT to the spawned CLI goes through libc::kill"
)]

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_core::{DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID};
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::manager;
use tisvlay_protocol::rpc;
use tokio::io::{AsyncBufReadExt, BufReader};

/// How long `eventually` keeps polling a getter: a set's ack and the
/// state update it causes travel separately, so a read right after a
/// write can still see the old value.
const SETTLE: Duration = Duration::from_secs(10);

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

    fn cli(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_tisvlay"));
        command
            .env_remove("COMPLETE")
            .env("TOKIO_WORKER_THREADS", "2")
            .arg("--socket")
            .arg(&self.socket)
            .arg("--user-socket")
            .arg(&self.user_socket)
            .args(["--site", "test"])
            .args(args)
            .kill_on_drop(true);
        command
    }

    async fn command(&self, args: &[&str]) -> Output {
        tokio::time::timeout(Duration::from_secs(15), self.cli(args).output())
            .await
            .expect("CLI command timed out")
            .expect("run tisvlay")
    }

    /// Run a command that must succeed: its stdout and stderr.
    async fn run(&self, args: &[&str]) -> (String, String) {
        let output = self.command(args).await;
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{args:?} failed: {stderr}");
        (String::from_utf8(output.stdout).unwrap(), stderr)
    }

    async fn success(&self, args: &[&str]) -> String {
        self.run(args).await.0
    }

    /// Run a writer, which prints nothing on success.
    async fn silent(&self, args: &[&str]) {
        assert_eq!(self.success(args).await, "", "{args:?} printed output");
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

    /// Poll a getter until it prints `expected`.
    async fn eventually(&self, args: &[&str], expected: &str) {
        let deadline = tokio::time::Instant::now() + SETTLE;
        loop {
            let printed = self.success(args).await;
            if printed.trim() == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{args:?} printed {printed:?}, expected {expected:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Pair a mock TV that accepts at once; `pair` prints its UUID.
    async fn pair(&self, name: &str, address: &str) -> String {
        let params = json!({ "address": address, "pairing": "accept" }).to_string();
        let id = self
            .success(&["pair", name, "--vendor", "mock", "--params", &params])
            .await;
        let id = id.trim().to_string();
        uuid::Uuid::parse_str(&id).unwrap();
        id
    }

    /// Enroll a mock TV without pairing, straight over the add-tv rpc (the
    /// CLI only pairs).
    async fn add(&self, name: &str, config: Value) -> String {
        let session =
            zenoh::open(tisvlay_protocol::bootstrap::client_config(&self.socket).unwrap())
                .await
                .unwrap();
        let request = rpc::AddTv {
            id: None,
            name: name.into(),
            vendor: "mock".into(),
            managed: true,
            config: serde_json::from_value(config).unwrap(),
        };
        let created = Caller::new(session, "test")
            .call::<manager::AddTv>(&(), &request)
            .await
            .unwrap();
        created.id.to_string()
    }

    /// Wait until `list` reports the TV's link down.
    async fn until_disconnected(&self, tv: &str) {
        let deadline = tokio::time::Instant::now() + SETTLE;
        while self.json(&["list"]).await[tv]["state/link"]["connected"] != false {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{tv} never disconnected"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tv_attributes_set_read_back_and_toggle() {
    let rig = CliDaemon::start().await;
    assert!(rig.success(&["status"]).await.contains("daemon"));
    assert!(rig.json(&["status"]).await.is_object());
    assert_eq!(rig.json(&["vendor"]).await[0]["id"], "mock");
    let vendor = rig.json(&["vendor", "mock"]).await;
    assert_eq!(vendor["id"], "mock");
    assert!(
        vendor["schema"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["name"] == "address")
    );
    assert!(rig.success(&["vendor", "mock"]).await.contains("address"));
    assert!(rig.success(&["list"]).await.contains("No TVs configured"));

    let tv = rig.pair("Office", "mock://office").await;
    let tv = tv.as_str();
    rig.eventually(&["power", tv], "on").await;
    assert_eq!(rig.json(&["power", tv]).await, "active");
    rig.eventually(&["screen", tv], "on").await;
    rig.eventually(&["volume", tv], "30").await;

    rig.silent(&["volume", tv, "64"]).await;
    rig.eventually(&["volume", tv], "64").await;
    assert_eq!(rig.json(&["volume", tv]).await, 64);
    rig.silent(&["backlight", tv, "24"]).await;
    rig.eventually(&["backlight", tv], "24").await;
    rig.silent(&["input", tv, "HDMI_2"]).await;
    rig.eventually(&["input", tv], "HDMI_2").await;
    assert_eq!(rig.json(&["input", tv]).await, "HDMI_2");
    rig.silent(&["mute", tv, "true"]).await;
    rig.eventually(&["mute", tv], "on").await;
    assert_eq!(rig.json(&["mute", tv]).await, true);
    assert_eq!(rig.success(&["mute", tv, "--toggle"]).await, "off\n");
    rig.eventually(&["mute", tv], "off").await;

    rig.silent(&["name", tv, "Renamed office"]).await;
    assert_eq!(rig.success(&["name", tv]).await, "Renamed office\n");
    rig.silent(&["managed", tv, "off"]).await;
    assert_eq!(rig.success(&["managed", tv]).await, "off\n");
    assert_eq!(rig.json(&["managed", tv]).await, false);

    let state = rig.json(&["list"]).await;
    assert_eq!(state[tv]["meta/identity"]["name"], "Renamed office");
    assert_eq!(state[tv]["state/volume"]["level"], 64);
    assert_eq!(state[tv]["state/backlight"]["level"], 24);
    let rendered = rig.success(&["list"]).await;
    assert!(rendered.contains("Renamed office"));
    assert!(rendered.contains("mock://office"));
    assert!(rendered.contains("[unmanaged]"));

    assert_eq!(rig.json(&["managed", tv, "--toggle"]).await, true);
    assert_eq!(rig.success(&["managed", tv]).await, "on\n");
    rig.silent(&["remove", tv]).await;
    assert!(rig.json(&["list"]).await.as_object().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_configuration_and_catalog_commands_round_trip() {
    let rig = CliDaemon::start().await;
    assert!(rig.success(&["vendor"]).await.contains("Mock"));
    assert!(
        rig.success(&["discover", "--vendor", "mock", "--timeout", "10ms"])
            .await
            .contains("nothing found")
    );
    assert_eq!(
        rig.json(&["discover", "--vendor", "mock", "--timeout", "10ms"])
            .await,
        json!([])
    );
    assert!(
        rig.json(&["profile", "system", "defaults"])
            .await
            .is_object()
    );

    let profile = rig.success(&["profile", "system", "create", "Work"]).await;
    let profile = profile.trim();
    uuid::Uuid::parse_str(profile).unwrap();
    rig.silent(&[
        "profile",
        "system",
        "update",
        profile,
        r#"{"name":"Work edited"}"#,
    ])
    .await;
    let duplicate = rig
        .success(&["profile", "system", "duplicate", profile, "Work copy"])
        .await;
    let duplicate = duplicate.trim();
    let catalog = rig.json(&["profile", "system", "list"]).await;
    assert_eq!(catalog[profile]["name"], "Work edited");
    assert_eq!(catalog[duplicate]["name"], "Work copy");
    assert!(
        rig.success(&["profile", "system", "list"])
            .await
            .contains("Work edited")
    );

    let tv = rig
        .add("Office", json!({ "address": "mock://office" }))
        .await;
    let tv = tv.as_str();
    let default = DEFAULT_SYSTEM_PROFILE_UUID.to_string();
    assert_eq!(rig.success(&["system-profile", tv]).await.trim(), default);
    rig.silent(&["system-profile", tv, profile]).await;
    assert_eq!(rig.success(&["system-profile", tv]).await.trim(), profile);
    assert_eq!(rig.json(&["system-profile", tv]).await, profile);
    assert!(
        rig.json(&["system-profile", tv, "--effective"])
            .await
            .is_object()
    );
    rig.silent(&["system-profile", tv, "--clear"]).await;
    assert_eq!(rig.success(&["system-profile", tv]).await.trim(), default);

    rig.silent(&[
        "config",
        tv,
        r#"{"name":"Configured office","pairing":"accept"}"#,
    ])
    .await;
    let config = rig.json(&["config", tv]).await;
    assert!(config.to_string().contains("Configured office"));
    assert!(rig.success(&["config", tv]).await.contains("mock://office"));

    assert_eq!(rig.success(&["primary-input", tv]).await, "");
    assert_eq!(rig.json(&["primary-input", tv]).await, Value::Null);
    rig.silent(&["primary-input", tv, "HDMI_1"]).await;
    assert_eq!(rig.success(&["primary-input", tv]).await, "HDMI_1\n");
    rig.silent(&["primary-input", tv, "--clear"]).await;
    assert_eq!(rig.success(&["primary-input", tv]).await, "");

    rig.silent(&["screen", tv, "off"]).await;
    rig.silent(&["screen", tv, "on"]).await;
    assert_eq!(rig.success(&["repair", tv]).await.trim(), tv);
    rig.silent(&["power", tv, "off"]).await;
    rig.silent(&["power", tv, "on"]).await;
    rig.silent(&["reconnect", tv]).await;
    rig.silent(&["profile", "system", "delete", duplicate])
        .await;
    assert!(
        rig.json(&["profile", "system", "list"])
            .await
            .get(duplicate)
            .is_none()
    );
    rig.silent(&["remove", tv]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_input_and_rpc_refusals_exit_unsuccessfully() {
    let rig = CliDaemon::start().await;
    let missing = uuid::Uuid::new_v4().to_string();
    rig.failure(&["volume", "not-a-uuid", "20"], "invalid value")
        .await;
    rig.failure(&["config", &missing, "[]"], "JSON object")
        .await;
    rig.failure(
        &["profile", "system", "create", "Work", "--body", "[]"],
        "JSON object",
    )
    .await;
    rig.failure(
        &["pair", "Office", "--vendor", "mock", "--params", "[]"],
        "JSON object",
    )
    .await;
    rig.failure(&["vendor", "missing-vendor"], "unknown-vendor")
        .await;
    rig.failure(&["remove", &missing], "not-found").await;
    rig.failure(&["volume", &missing], "not-found").await;
    rig.failure(&["power", &missing], "not-found").await;
    rig.failure(&["power", &missing, "--toggle"], "not-found")
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tv_that_never_connected_reads_off_and_reports_no_state() {
    let rig = CliDaemon::start().await;
    let tv = rig
        .add(
            "Unreachable",
            json!({ "address": "mock://unreachable", "refuse_connect": true }),
        )
        .await;
    let tv = tv.as_str();
    assert_eq!(rig.success(&["power", tv]).await, "off\n");
    assert_eq!(rig.success(&["screen", tv]).await, "off\n");
    rig.failure(&["volume", tv], "hasn't reported its volume")
        .await;
    rig.failure(&["mute", tv, "--toggle"], "disconnected").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnected_tv_reads_off_warns_on_stale_state_and_toggles_on() {
    let rig = CliDaemon::start().await;
    let tv = rig.pair("Den", "mock://den").await;
    let tv = tv.as_str();
    rig.eventually(&["volume", tv], "30").await;
    // The mock models a network TV: powering off drops its link.
    rig.silent(&["power", tv, "off"]).await;
    rig.until_disconnected(tv).await;

    assert_eq!(rig.success(&["power", tv]).await, "off\n");
    assert_eq!(rig.success(&["screen", tv]).await, "off\n");
    let (volume, warning) = rig.run(&["volume", tv]).await;
    assert_eq!(volume, "30\n");
    assert!(warning.contains("disconnected"), "{warning:?}");
    rig.failure(&["mute", tv, "--toggle"], "disconnected").await;

    // A toggle on a disconnected TV powers it on, through the wake path.
    assert_eq!(rig.success(&["power", tv, "--toggle"]).await, "on\n");
    rig.eventually(&["power", tv], "on").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn user_profiles_and_remote_sources_use_the_session_socket() {
    let rig = CliDaemon::with_session().await;
    assert!(rig.json(&["profile", "user", "defaults"]).await.is_object());
    let profile = rig
        .success(&["profile", "user", "create", "Personal"])
        .await;
    let profile = profile.trim();
    rig.silent(&[
        "profile",
        "user",
        "update",
        profile,
        r#"{"name":"Personal edited"}"#,
    ])
    .await;
    let duplicate = rig
        .success(&["profile", "user", "duplicate", profile, "Personal copy"])
        .await;
    let duplicate = duplicate.trim();
    let catalog = rig.json(&["profile", "user", "list"]).await;
    assert_eq!(catalog[profile]["name"], "Personal edited");
    assert_eq!(catalog[duplicate]["name"], "Personal copy");
    assert!(
        rig.success(&["profile", "user", "list"])
            .await
            .contains("Personal edited")
    );

    let tv = rig
        .add("Office", json!({ "address": "mock://office" }))
        .await;
    let tv = tv.as_str();
    let default = DEFAULT_USER_PROFILE_UUID.to_string();
    assert_eq!(rig.success(&["user-profile", tv]).await.trim(), default);
    rig.silent(&["user-profile", tv, profile]).await;
    rig.eventually(&["user-profile", tv], profile).await;
    assert!(
        rig.json(&["user-profile", tv, "--effective"])
            .await
            .is_object()
    );
    rig.silent(&["user-profile", tv, "--clear"]).await;
    rig.eventually(&["user-profile", tv], &default).await;
    let missing = uuid::Uuid::new_v4().to_string();
    rig.failure(&["user-profile", &missing], "not-found").await;
    rig.silent(&["profile", "user", "delete", duplicate]).await;
    assert!(
        rig.json(&["profile", "user", "list"])
            .await
            .get(duplicate)
            .is_none()
    );

    rig.silent(&["remote-session", "start", "--source", "cli-test"])
        .await;
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
    rig.silent(&["remote-session", "stop", "--source", "cli-test"])
        .await;
    assert!(
        !rig.json(&["remote-session", "status"])
            .await
            .to_string()
            .contains("cli-test")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn discover_watch_streams_until_interrupted() {
    let rig = CliDaemon::start().await;
    let mut child = rig
        .cli(&["discover", "--watch", "--vendor", "mock"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    let banner = tokio::time::timeout(SETTLE, stderr.next_line())
        .await
        .expect("no discovery banner")
        .unwrap()
        .unwrap();
    assert!(banner.contains("ctrl-c stops"), "{banner:?}");
    let pid = libc::pid_t::try_from(child.id().unwrap()).unwrap();
    // SAFETY: `pid` is our own live child (it just printed its banner and
    // we hold its handle, so it can't have been reaped); SIGINT is the
    // ctrl-c the verb waits for.
    let sent = unsafe { libc::kill(pid, libc::SIGINT) };
    assert_eq!(sent, 0);
    let status = tokio::time::timeout(SETTLE, child.wait())
        .await
        .expect("discover --watch didn't stop on SIGINT")
        .unwrap();
    assert!(status.success());
    let rest = stderr.next_line().await.unwrap().unwrap_or_default();
    assert_eq!(rest, "stopped");
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
