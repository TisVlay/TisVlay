//! Private `dbus-daemon` spawner for tests.
//!
//! [`PrivateBus::shared`] is one private `dbus-daemon` per test process,
//! and every test in that process may connect to it. Under nextest that's
//! one bus per test (process-per-test); under plain `cargo test` the tests
//! of a binary share one, each on its own thread. No two processes ever
//! share a bus. Tests that own well-known names take a
//! [`PrivateBus::isolated`] bus of their own instead.
//!
//! # Lifecycle
//!
//! The shared daemon is spawned lazily on first access and left running
//! for the lifetime of the test process; an isolated one is killed and
//! reaped when its handle drops. To avoid leaking the child if the run
//! is killed abruptly (SIGINT, panic before drop, etc.), we set
//! `PR_SET_PDEATHSIG = SIGKILL` in a `pre_exec` hook so the kernel tears
//! it down when its parent dies. The kernel's "parent" there is the
//! *thread* that forked the child, so the fork happens on a dedicated
//! thread that lives as long as the process — forking from whichever
//! test touched the bus first would kill the daemon when that test's
//! thread exits, under `cargo test`'s thread-per-test model.
//!
//! # Bus address
//!
//! `dbus-daemon --session --print-address` writes the `unix:path=...`
//! line to stdout; we capture the first line via a piped stdout, then
//! discard the rest. Tests connect via
//! [`zbus::connection::Builder::address`] so they bypass zbus's
//! per-process `OnceCell` connection cache.

#![expect(
    unsafe_code,
    reason = "a pre_exec hook sets PR_SET_PDEATHSIG in the forked dbus-daemon"
)]

use parking_lot::Mutex;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use tempfile::TempDir;
use tracing::{debug, instrument};

/// Handle to a private `dbus-daemon`; see [`shared`](Self::shared) and
/// [`isolated`](Self::isolated).
#[derive(Debug)]
pub struct PrivateBus {
    address: String,
    /// Config file + socket dir live here; removed after `Drop` has reaped
    /// the daemon. The shared `LazyLock` instance never drops, so its
    /// tempdir is left behind when the test process exits.
    _workdir: TempDir,
    /// Killed and reaped on drop; pdeathsig covers the abrupt-exit paths.
    child: Mutex<std::process::Child>,
    /// Holds the spawner thread open; dropping it lets the thread return.
    _release: std::sync::mpsc::Sender<()>,
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        // Reap before the fields drop: the workdir must outlive the
        // daemon, and an unreaped child lingers as a zombie in a
        // long-lived `cargo test` process.
        let child = self.child.get_mut();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl PrivateBus {
    /// Returns a reference to the shared private bus for this test process,
    /// spawning it on first call.
    ///
    /// # Panics
    ///
    /// Panics if `dbus-daemon` is not on `PATH` or fails to start. Tests
    /// that need this harness are expected to run in an environment where
    /// `dbus-daemon` is available (nix dev shell, CI image, etc.).
    pub fn shared() -> &'static PrivateBus {
        static BUS: LazyLock<PrivateBus> = LazyLock::new(PrivateBus::isolated);
        &BUS
    }

    /// A private bus of the caller's own, for tests that own well-known
    /// names (a [`FakeLogind`](crate::fake_logind::FakeLogind), a fake
    /// ScreenSaver). Name ownership is bus-wide, so such tests sharing one
    /// bus in a process — plain `cargo test` runs a binary's tests
    /// concurrently in one process — would claim each other's names.
    /// Dropping it ends the daemon.
    ///
    /// # Panics
    ///
    /// Same as [`shared`](Self::shared).
    #[expect(
        clippy::panic,
        reason = "test infrastructure: a missing or failing dbus-daemon fails the test with a clear message"
    )]
    pub fn isolated() -> PrivateBus {
        match PrivateBus::spawn() {
            Ok(bus) => bus,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => panic!(
                "dbus-daemon not found on PATH: the tisvlay-test D-Bus fakes need it \
                 (the Nix dev shell provides it; elsewhere install the dbus package)"
            ),
            Err(e) => panic!("failed to spawn private dbus-daemon: {e}"),
        }
    }

    /// The bus address as a `unix:path=...` string, suitable for
    /// [`zbus::connection::Builder::address`].
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Build a fresh zbus connection to this bus.
    ///
    /// Uses `Builder::address(...)` so the connection bypasses zbus's
    /// per-process `session()` / `system()` `OnceCell` cache.
    pub async fn connect(&self) -> zbus::Result<zbus::Connection> {
        zbus::connection::Builder::address(self.address.as_str())?
            .build()
            .await
    }

    #[instrument(skip_all)]
    fn spawn() -> std::io::Result<PrivateBus> {
        let workdir = crate::tmp::socket_tempdir();

        // Minimal session bus config pinned to a socket inside our tempdir.
        // No services are activated; tests register their own objects on
        // the bus directly.
        let socket_path = workdir.path().join("bus.sock");
        let config_path = workdir.path().join("bus.conf");
        let config = format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={socket}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
            socket = socket_path.display(),
        );
        std::fs::write(&config_path, config)?;

        // Scrub env vars that would leak the outer session's bus into
        // the child's view of itself.
        let mut cmd = Command::new("dbus-daemon");
        cmd.arg(format!("--config-file={}", config_path.display()))
            .arg("--print-address")
            .arg("--nofork")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
            .env_remove("DBUS_STARTER_ADDRESS")
            .env_remove("DBUS_STARTER_BUS_TYPE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let die_with_parent = || {
            // SAFETY: prctl(PR_SET_PDEATHSIG) takes plain integers and is
            // async-signal-safe.
            if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        };
        // SAFETY: pre_exec runs after fork() in the child before exec(); the
        // hook only calls prctl and touches no shared state.
        unsafe { std::os::unix::process::CommandExt::pre_exec(&mut cmd, die_with_parent) };

        // Fork from a thread that lives as long as the bus handle, so the
        // pdeathsig fires when the bus is dropped (never, for the shared
        // one) or the process dies — not when the calling test's thread
        // ends.
        let (spawned_tx, spawned_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("tisvlay-test-bus".into())
            .spawn(move || {
                let _ = spawned_tx.send(cmd.spawn());
                // Returns once the bus handle drops its sender.
                let _ = release_rx.recv();
            })?;
        let mut child = spawned_rx
            .recv()
            .map_err(|_| std::io::Error::other("bus spawner thread exited early"))??;
        let stdout = child
            .stdout
            .take()
            .expect("stdout was piped but missing after spawn");

        // Read exactly one line — the bus address — then let the daemon
        // continue running with its stdout unconsumed. dbus-daemon only
        // prints one line to stdout when --print-address is set.
        let mut reader = BufReader::new(stdout);
        let mut address = String::new();
        reader.read_line(&mut address)?;
        let address = address.trim().to_string();

        if address.is_empty() {
            return Err(std::io::Error::other(
                "dbus-daemon exited before printing bus address",
            ));
        }

        debug!(address = %address, "private dbus-daemon ready");

        // Tiny poll: make sure the socket is listening before we return.
        // dbus-daemon prints the address immediately after binding, but
        // give it a moment in case of filesystem lag.
        for _ in 0..20 {
            if socket_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        Ok(PrivateBus {
            address,
            _workdir: workdir,
            child: Mutex::new(child),
            _release: release_tx,
        })
    }
}
