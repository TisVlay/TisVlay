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
//! Gated on the `mock` Cargo feature; production builds never
//! include this module. Each test calls [`TestDaemon::with_tv`] (or
//! similar) and gets a fully wired daemon with no shared state — the
//! shared `tisvlay-test-support::PrivateBus` provides binary-level
//! isolation, and unique TV IDs prevent in-binary collisions.
//!
//! # Example
//!
//! ```ignore
//! use tisvlayd::test_harness::TestDaemon;
//! use tisvlay_core::dbus_client::DaemonClient;
//!
//! #[tokio::test]
//! async fn screen_off_propagates_to_dbus() {
//!     let daemon = TestDaemon::with_one_mock_tv("living_room").await;
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
use tokio::task::JoinHandle;
use tracing::instrument;

use crate::dbus::{build_dbus_connection, BusKind, ManagerService};
use crate::state_forwarder::state_change_forwarder;
use crate::tv_manager::TvManager;

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
    /// Build a daemon with a single mock TV registered under `tv_id`.
    ///
    /// The TV is `managed`, has no `primary_input` set, and uses the
    /// default mock vendor config (`mock://{tv_id}`). The mock backend
    /// is wired through `create_test_registry` so the returned
    /// `MockHandle` shares state with the daemon's connector.
    #[instrument]
    pub async fn with_one_mock_tv(tv_id: &str) -> anyhow::Result<Self> {
        let (registry, handle) = create_test_registry();
        let registry = Arc::new(registry);
        let registry_for_service = registry.clone();

        // Build a SystemConfig with one mock TV. The vendor_config
        // entry uses the mock://{tv_id} address so it matches the
        // address tests would use to drive pairing via the handle.
        let mut tv = TvConfig::new(tv_id, tv_id, "mock", true);
        let mut vendor_config = HashMap::new();
        let address = format!("mock://{}", tv_id);
        vendor_config.insert("address".to_string(), toml::Value::String(address.clone()));
        tv.vendor_config.insert(
            "mock".to_string(),
            toml::Value::Table(vendor_config.into_iter().collect()),
        );

        let mut config = SystemConfig::default();
        config.tv.push(tv);

        // Persist the config in a per-test tempdir so reload_config
        // calls don't try to write to /var/lib/tisvlay.
        let config_dir = tempfile::Builder::new()
            .prefix("tisvlay-test-daemon-")
            .tempdir()?;
        let config_path = config_dir.path().join("config.toml");
        config.save_to(&config_path)?;

        let (tv_manager, state_change_rx) = TvManager::new(config, config_path, registry);
        let tv_ids: Vec<String> = tv_manager.list_tv_ids().await;
        let manager = Arc::new(tv_manager);

        let manager_service = ManagerService::new(manager.clone(), registry_for_service);

        let bus = tisvlay_test_support::PrivateBus::shared();
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
