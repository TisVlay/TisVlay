//! Test harness for session service integration tests.
//!
//! Constructs a [`TestSession`] backed by a [`TestDaemon`] on the
//! shared private bus, with a [`FakeScreenSaver`] for injecting
//! lock/unlock events.
//!
//! # Example
//!
//! ```ignore
//! use tisvlay_harness::session::TestSession;
//! use tisvlay_session::__internals::SessionEvent;
//!
//! #[tokio::test]
//! async fn session_lock_dispatches_screen_off() {
//!     let session = TestSession::new("my_tv").await.unwrap();
//!     session.inject_event(SessionEvent::SessionLocked);
//!     // ... assert state via session.daemon() ...
//! }
//! ```

use std::sync::Arc;

use tisvlay_core::DaemonClient;
use tisvlay_session::__internals::{
    SessionEvent, SessionState, event_handler, register_session_dbus_on,
};
use tisvlay_test::fake_screensaver::FakeScreenSaver;
use tokio::sync::{RwLock, mpsc};

use crate::daemon::TestDaemon;

/// A fully wired session service instance on the shared private bus.
///
/// Backed by a [`TestDaemon`] so there's a real daemon to talk to.
/// The [`FakeScreenSaver`] is registered on the same bus for
/// round-trip signal tests. For simpler tests, use
/// [`TestSession::inject_event`] to push directly into the event
/// channel.
#[derive(Debug)]
pub struct TestSession {
    daemon: TestDaemon,
    daemon_client: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    screensaver: FakeScreenSaver,
    _session_dbus_conn: zbus::Connection,
    handler_task: tokio::task::JoinHandle<()>,
}

impl TestSession {
    /// Build a session service backed by a daemon with one managed mock TV.
    ///
    /// The TV uses the default system profile. The session loads state
    /// from the daemon, registers the Session D-Bus interface, and
    /// spawns the event handler — all on the shared private bus.
    ///
    /// User config path is `None`, which means `SessionState::load` and
    /// the Session D-Bus interface fall back to XDG defaults
    /// (`~/.config/tisvlay/config.toml`). For tests that exercise
    /// profile mutation without polluting the developer's real user
    /// config, use [`TestSession::with_user_config_path`].
    pub async fn new(tv_id: &str) -> anyhow::Result<Self> {
        Self::build(tv_id, None).await
    }

    /// Like [`TestSession::new`] but pins the user config to a
    /// specific path — typically a file inside a `tempfile::TempDir`
    /// the test owns. Avoids reading or writing
    /// `~/.config/tisvlay/config.toml` for tests that call
    /// `SetTvUserProfile` or `ReloadConfig`.
    pub async fn with_user_config_path(
        tv_id: &str,
        config_path: std::path::PathBuf,
    ) -> anyhow::Result<Self> {
        Self::build(tv_id, Some(config_path)).await
    }

    async fn build(tv_id: &str, config_path: Option<std::path::PathBuf>) -> anyhow::Result<Self> {
        let daemon = TestDaemon::with_one_mock_tv(tv_id).await?;

        // Connect the daemon and reconnect so the TV is online. The
        // daemon keys TVs by UUID, so translate the fixture name to
        // its derived UUID first.
        let tv_uuid = crate::daemon::test_tv_uuid(tv_id).to_string();
        daemon.manager().reconnect(&tv_uuid).await?;

        let bus = tisvlay_test::PrivateBus::shared();

        // Build a DaemonClient on the same private bus.
        let client_conn = bus.connect().await?;
        let daemon_client = Arc::new(DaemonClient::from_connection(client_conn).await?);

        // Load session state from the daemon.
        let session_state = SessionState::load(&daemon_client, config_path.as_ref()).await?;
        let state = Arc::new(RwLock::new(session_state));

        // Register session D-Bus interface on the private bus.
        let session_dbus_conn = register_session_dbus_on(
            tisvlay_core::BusKind::Address(bus.address()),
            state.clone(),
            daemon_client.clone(),
            config_path,
        )
        .await?;

        // Register FakeScreenSaver on a separate connection.
        let ss_conn = bus.connect().await?;
        let screensaver = FakeScreenSaver::register(&ss_conn).await?;

        // Event channel + handler.
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let handler_state = state.clone();
        let handler_daemon = daemon_client.clone();
        let handler_task = tokio::spawn(async move {
            event_handler(event_rx, handler_state, handler_daemon).await;
        });

        Ok(Self {
            daemon,
            daemon_client,
            state,
            event_tx,
            screensaver,
            _session_dbus_conn: session_dbus_conn,
            handler_task,
        })
    }

    /// The underlying test daemon.
    pub fn daemon(&self) -> &TestDaemon {
        &self.daemon
    }

    /// The DaemonClient connected to the private bus.
    pub fn client(&self) -> &Arc<DaemonClient> {
        &self.daemon_client
    }

    /// The session state (profile resolutions, TV list, etc.).
    pub fn state(&self) -> &Arc<RwLock<SessionState>> {
        &self.state
    }

    /// The fake ScreenSaver for round-trip signal tests.
    pub fn screensaver(&self) -> &FakeScreenSaver {
        &self.screensaver
    }

    /// Inject a session event directly into the event handler.
    ///
    /// Bypasses `SessionMonitor` for deterministic unit-style tests.
    pub fn inject_event(&self, event: SessionEvent) {
        self.event_tx
            .send(event)
            .expect("event handler should be running");
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        self.handler_task.abort();
    }
}
