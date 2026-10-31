#![expect(
    unsafe_code,
    reason = "stopping the services sends SIGINT through libc::kill"
)]

//! `tisvlay service daemon` and `tisvlay service session` as real
//! processes, the way their systemd units start them. The workflow tests
//! boot the stacks in-process, so this is what covers the service entry
//! points, the CLI's dispatch to them (the session's included, which must
//! run before any thread exists) and the logging init.

use std::process::Stdio;
use std::time::Duration;

/// How long a freshly started service gets to answer.
const STARTUP: Duration = Duration::from_secs(20);

/// The `tisvlay` binary, kept off the developer's desktop: no session bus,
/// no compositor, and nothing logged to the journal. The coverage
/// runtime's variables pass through, so the services still record.
fn tisvlay() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_tisvlay"));
    command
        .env_remove("COMPLETE")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env("RUST_LOG", "off")
        .env("TOKIO_WORKER_THREADS", "2")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

/// A running `tisvlay service …` process.
struct Service(tokio::process::Child);

impl Service {
    fn spawn(args: &[&str]) -> Self {
        let child = tisvlay()
            .arg("service")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self(child)
    }

    /// Stop it as ctrl-c would; it must exit cleanly, not crash or hang.
    async fn stop(self) {
        let pid =
            libc::pid_t::try_from(self.0.id().expect("the service is still running")).unwrap();
        // SAFETY: `pid` is our own child, not yet reaped while we hold it, so
        // the id can't have been reused by another process.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
        let output = tokio::time::timeout(Duration::from_secs(15), self.0.wait_with_output())
            .await
            .expect("the service didn't exit after SIGINT")
            .unwrap();
        assert!(
            output.status.success(),
            "the service exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Run a CLI command until it succeeds, or fail after `STARTUP`.
async fn until_ok(args: &[&str]) {
    let deadline = tokio::time::Instant::now() + STARTUP;
    loop {
        let output = tisvlay().args(args).output().await.unwrap();
        if output.status.success() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{args:?} never succeeded: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_and_session_run_answer_and_stop_cleanly() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("daemon.sock");
    let user_socket = directory.path().join("session.sock");
    let (socket, user_socket) = (socket.to_str().unwrap(), user_socket.to_str().unwrap());

    let daemon = Service::spawn(&[
        "daemon",
        "--socket",
        socket,
        "--site",
        "svc",
        "--ssdp-port",
        "0",
        "--ephemeral",
        "--seat-access=false",
    ]);
    until_ok(&["--socket", socket, "--site", "svc", "status"]).await;

    // Port 1 refuses at once, so a Sunshine running on this machine can't
    // arm the session's stream detector.
    let session = Service::spawn(&[
        "session",
        "--system-socket",
        socket,
        "--user-socket",
        user_socket,
        "--site",
        "svc",
        "--ephemeral",
        "--sunshine-port",
        "1",
    ]);
    until_ok(&[
        "--socket",
        socket,
        "--user-socket",
        user_socket,
        "--site",
        "svc",
        "profile",
        "user",
        "list",
    ])
    .await;

    session.stop().await;
    daemon.stop().await;
}
