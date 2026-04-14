//! Test harness for daemon integration tests.
//!
//! Constructs a [`TvManager`] populated with mock TVs, registers the
//! full daemon D-Bus surface (`Manager1`, `ObjectManager`, every
//! `Tv1.*` interface) onto a private bus, spawns the state-change
//! forwarder, and returns a [`TestDaemon`] holding everything tests
//! need to drive the daemon end-to-end:
//!
//! - the [`MockHandle`] for publishing/pairing TVs
//! - a `zbus::Connection` to the same private bus, for building a
//!   `DaemonClient::from_connection` (or any custom proxy work)
//!
//! Each test calls [`TestDaemon::with_one_mock_tv`] (or
//! [`TestDaemon::with_config`] for custom setups) and gets a fully
//! wired daemon with no shared state — the shared
//! [`tisvlay_test::PrivateBus`] provides binary-level isolation, and
//! unique TV IDs prevent in-binary collisions.
//!
//! # Example
//!
//! ```ignore
//! use tisvlay_harness::daemon::TestDaemon;
//! use tisvlay_core::DaemonClient;
//!
//! #[tokio::test]
//! async fn screen_off_propagates_to_dbus() {
//!     let daemon = TestDaemon::with_one_mock_tv("living_room").await.unwrap();
//!     let client = DaemonClient::from_connection(daemon.connection().clone())
//!         .await
//!         .unwrap();
//!     // ... drive client, assert state via daemon.handle() ...
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use tisvlay_backends::{create_test_registry, MockHandle};
use tisvlay_core::{SystemConfig, TvConfig};
use tisvlayd::__internals::{
    build_dbus_connection, state_change_forwarder, BusKind, ManagerService, TvManager,
};
use tokio::task::JoinHandle;
use tracing::instrument;

/// Build a minimal `TvConfig` for a mock TV with standard vendor
/// config. Use this in tests that construct a `SystemConfig` by hand.
pub fn mock_tv_config(tv_id: &str, managed: bool, primary_input: Option<&str>) -> SystemConfig {
    let mut tv = TvConfig::new(tv_id, tv_id, "mock", managed);
    if let Some(input) = primary_input {
        tv = tv.with_primary_input(input);
    }
    let mut vendor_table = HashMap::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String(format!("mock://{}", tv_id)),
    );
    tv.vendor_config.insert(
        "mock".to_string(),
        toml::Value::Table(vendor_table.into_iter().collect()),
    );

    let mut config = SystemConfig::default();
    config.tv.push(tv);
    config
}

/// A fully wired daemon instance running on the shared private bus.
///
/// Holds the connection, the TV manager, and the mock handle so
/// tests can drive any of the three. Drop semantics: the spawned
/// state-change-forwarder task is aborted on drop and the connection
/// is released, which unregisters the well-known name from the bus.
pub struct TestDaemon {
    connection: zbus::Connection,
    manager: Arc<TvManager>,
    handle: MockHandle,
    forwarder: JoinHandle<()>,
    _config_dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Build a daemon from a fully custom `SystemConfig`.
    ///
    /// The registry and mock handle come from `create_test_registry()`,
    /// so the mock connector inside the daemon and the returned handle
    /// share state. Callers can pre-configure any combination of TVs,
    /// profiles, managed flags, primary_input, etc. in the config.
    pub async fn with_config(config: SystemConfig) -> anyhow::Result<Self> {
        let (registry, handle) = create_test_registry();
        let registry = Arc::new(registry);
        let registry_for_service = registry.clone();

        let config_dir = tempfile::Builder::new()
            .prefix("tisvlay-test-daemon-")
            .tempdir()?;
        let config_path = config_dir.path().join("config.toml");
        config.save_to(&config_path)?;

        let (tv_manager, state_change_rx) = TvManager::new(config, config_path, registry);
        let tv_ids: Vec<String> = tv_manager.list_tv_ids().await;
        let manager = Arc::new(tv_manager);

        let manager_service = ManagerService::new(manager.clone(), registry_for_service);

        let bus = tisvlay_test::PrivateBus::shared();
        let connection = build_dbus_connection(
            BusKind::Address(bus.address()),
            manager_service,
            &tv_ids,
            manager.clone(),
        )
        .await?;

        let forwarder = tokio::spawn(state_change_forwarder(
            connection.clone(),
            state_change_rx,
            manager.clone(),
        ));

        Ok(Self {
            connection,
            manager,
            handle,
            forwarder,
            _config_dir: config_dir,
        })
    }

    /// Build a daemon with a single mock TV registered under `tv_id`.
    ///
    /// The TV is `managed`, has no `primary_input` set, and uses the
    /// default mock vendor config (`mock://{tv_id}`). The mock backend
    /// is wired through `create_test_registry` so the returned
    /// `MockHandle` shares state with the daemon's connector.
    #[instrument]
    pub async fn with_one_mock_tv(tv_id: &str) -> anyhow::Result<Self> {
        Self::with_config(mock_tv_config(tv_id, true, None)).await
    }

    /// The zbus connection bound to the private test bus.
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }

    /// The TV manager — direct access for assertions on internal state.
    pub fn manager(&self) -> &Arc<TvManager> {
        &self.manager
    }

    /// The mock handle for driving discovery/pairing outcomes.
    pub fn handle(&self) -> &MockHandle {
        &self.handle
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}
