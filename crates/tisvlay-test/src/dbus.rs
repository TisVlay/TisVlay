//! Private `dbus-daemon` spawner for tests.
//!
//! Each test binary spawns exactly one private `dbus-daemon` process
//! (via [`PrivateBus::shared`]) and every test in that binary connects
//! to it. Binary-level isolation is provided by nextest's
//! process-per-binary model — no two test binaries ever share a bus.
//!
//! # Lifecycle
//!
//! The daemon is spawned lazily on first access and left running for
//! the lifetime of the test binary. To avoid leaking the child if
//! `cargo test` is killed abruptly (SIGINT, panic before drop, etc.),
//! we set `PR_SET_PDEATHSIG = SIGKILL` in a `pre_exec` hook so the
//! kernel tears it down automatically when the test process dies.
//!
//! # Bus address
//!
//! `dbus-daemon --session --print-address` writes the `unix:path=...`
//! line to stdout; we capture the first line via a piped stdout, then
//! discard the rest. Tests connect via
//! [`zbus::connection::Builder::address`] so they bypass zbus's
//! per-process `OnceCell` connection cache.

use parking_lot::Mutex;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use tempfile::TempDir;
use tracing::{debug, instrument};

/// Handle to a private `dbus-daemon` spawned for a single test binary.
#[derive(Debug)]
pub struct PrivateBus {
    address: String,
    /// Config file + socket dir live here; dropped when the bus is dropped.
    /// For the shared `LazyLock` instance this never drops, which is fine —
    /// the kernel reclaims the tempdir when the test process exits.
    _workdir: TempDir,
    /// Kept alive so the child stays reachable for explicit kill if needed.
    /// pdeathsig is the primary cleanup path.
    _child: Mutex<std::process::Child>,
}

impl PrivateBus {
    /// Returns a reference to the shared private bus for this test binary,
    /// spawning it on first call.
    ///
    /// # Panics
    ///
    /// Panics if `dbus-daemon` is not on `PATH` or fails to start. Tests
    /// that need this harness are expected to run in an environment where
    /// `dbus-daemon` is available (nix dev shell, CI image, etc.).
    pub fn shared() -> &'static PrivateBus {
        static BUS: LazyLock<PrivateBus> =
            LazyLock::new(|| PrivateBus::spawn().expect("failed to spawn private dbus-daemon"));
        &BUS
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
        let workdir = tempfile::Builder::new()
            .prefix("tisvlay-test-bus-")
            .tempdir()?;

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

        // SAFETY: pre_exec runs after fork() in the child before exec().
        // prctl is async-signal-safe, and we don't touch any shared state.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = cmd.spawn()?;
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
            _child: Mutex::new(child),
        })
    }
}
