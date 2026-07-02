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

use tisvlay_backends::{MockHandle, create_test_registry};
use tisvlay_core::{SystemConfig, TvConfig};
use tisvlayd::__internals::{
    BusKind, ManagerService, TvManager, build_dbus_connection, state_change_forwarder,
    state_poll_loop,
};
use tokio::task::JoinHandle;
use tracing::instrument;
use uuid::Uuid;

/// Derive a stable UUID from a test fixture identifier so that
/// `mock_tv_config("living_room", ..)` produces the same UUID every
/// run. Tests rely on this for reproducible D-Bus paths and config
/// fixtures.
pub fn test_tv_uuid(tv_id: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, tv_id.as_bytes())
}

/// Build a minimal `TvConfig` for a mock TV with standard vendor
/// config. Use this in tests that construct a `SystemConfig` by hand.
///
/// `tv_id` is a human-readable test fixture name (e.g. `"living_room"`)
/// used as the TV's `name` and to derive a deterministic UUID via
/// [`test_tv_uuid`].
pub fn mock_tv_config(tv_id: &str, managed: bool, primary_input: Option<&str>) -> SystemConfig {
    let mut config = SystemConfig::default();
    config.tv.push(build_mock_tv(tv_id, managed, primary_input));
    config
}

/// Build a `SystemConfig` containing one managed mock TV per entry in
/// `tv_ids`, in the given order. Each TV uses default profile, no
/// primary input, and the canonical `mock://<id>` vendor address.
///
/// Used by reload-path regression tests that need to seed an initial
/// config with N TVs and then mutate it on disk.
pub fn mock_config_with_tvs(tv_ids: &[&str]) -> SystemConfig {
    let mut config = SystemConfig::default();
    for tv_id in tv_ids {
        config.tv.push(build_mock_tv(tv_id, true, None));
    }
    config
}

fn build_mock_tv(tv_id: &str, managed: bool, primary_input: Option<&str>) -> TvConfig {
    build_mock_tv_inner(tv_id, managed, primary_input, false)
}

/// Build a `SystemConfig` with one managed, poll-only mock TV.
///
/// The mock's vendor config sets `disable_subscription = true`, so its
/// `subscribe_state` returns `NotSupported` and the daemon treats it
/// like a real poll-only backend (e.g. a projector): no push channel,
/// state refreshed via the periodic `state_poll_loop` / `get_state`
/// path and the reliable-dispatch active-verify path.
pub fn mock_tv_config_poll_only(
    tv_id: &str,
    managed: bool,
    primary_input: Option<&str>,
) -> SystemConfig {
    let mut config = SystemConfig::default();
    config
        .tv
        .push(build_mock_tv_inner(tv_id, managed, primary_input, true));
    config
}

/// Build a `SystemConfig` with one managed mock TV whose backend reports
/// `reachable_in_standby` (CEC-style): its bus stays live in standby, so
/// the daemon keeps the connection — and `Connected` — across a power-off,
/// treating power and connection as separate axes.
pub fn mock_tv_config_reachable_in_standby(tv_id: &str, managed: bool) -> SystemConfig {
    let mut tv = TvConfig::with_uuid(test_tv_uuid(tv_id), tv_id, "mock", managed);
    let mut vendor_table = HashMap::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String(format!("mock://{}", tv_id)),
    );
    vendor_table.insert(
        "reachable_in_standby".to_string(),
        toml::Value::Boolean(true),
    );
    tv.vendor_config.insert(
        "mock".to_string(),
        toml::Value::Table(vendor_table.into_iter().collect()),
    );
    let mut config = SystemConfig::default();
    config.tv.push(tv);
    config
}

fn build_mock_tv_inner(
    tv_id: &str,
    managed: bool,
    primary_input: Option<&str>,
    disable_subscription: bool,
) -> TvConfig {
    let mut tv = TvConfig::with_uuid(test_tv_uuid(tv_id), tv_id, "mock", managed);
    if let Some(input) = primary_input {
        tv = tv.with_primary_input(input);
    }
    let mut vendor_table = HashMap::new();
    vendor_table.insert(
        "address".to_string(),
        toml::Value::String(format!("mock://{}", tv_id)),
    );
    if disable_subscription {
        vendor_table.insert(
            "disable_subscription".to_string(),
            toml::Value::Boolean(true),
        );
    }
    tv.vendor_config.insert(
        "mock".to_string(),
        toml::Value::Table(vendor_table.into_iter().collect()),
    );
    tv
}

/// A fully wired daemon instance running on the shared private bus.
///
/// Holds the connection, the TV manager, and the mock handle so
/// tests can drive any of the three. Drop semantics: the spawned
/// state-change-forwarder task is aborted on drop and the connection
/// is released, which unregisters the well-known name from the bus.
#[derive(Debug)]
pub struct TestDaemon {
    connection: zbus::Connection,
    manager: Arc<TvManager>,
    handle: MockHandle,
    forwarder: JoinHandle<()>,
    /// Background poll-only state refresh loop. Spawned so tests match
    /// the production daemon's wiring; poll-only mock TVs (via the
    /// `disable_subscription` knob) get periodic refreshes. Tests that
    /// need a deterministic single pass call `manager().poll_state_once()`
    /// directly rather than waiting on the loop's interval.
    poll_loop: JoinHandle<()>,
    _config_dir: tempfile::TempDir,
}

impl TestDaemon {
    /// Build a daemon from a fully custom `SystemConfig`.
    ///
    /// The registry and mock handle come from `create_test_registry()`,
    /// so the mock connector inside the daemon and the returned handle
    /// share state. Callers can pre-configure any combination of TVs,
    /// profiles, managed flags, primary_input, etc. in the config.
    pub async fn with_config(mut config: SystemConfig) -> anyhow::Result<Self> {
        let (registry, handle) = create_test_registry();
        let registry = Arc::new(registry);
        let registry_for_service = registry.clone();

        let config_dir = tempfile::Builder::new()
            .prefix("tisvlay-test-daemon-")
            .tempdir()?;
        let config_path = config_dir.path().join("config.toml");
        // Mirror SystemConfig::load_from's invariants: the reserved
        // default profiles must exist before TvManager constructs.
        // Without this, tests that hand-build a `SystemConfig::default()`
        // would have empty `system.profiles` / `user.profiles` maps,
        // breaking the `Tv1.SystemProfile` setter (which validates
        // against the catalog).
        config.system.ensure_default();
        config.user.ensure_default();
        config.save_to(&config_path)?;

        let (tv_manager, state_change_rx) = TvManager::new(config, config_path, registry);
        let tv_ids: Vec<String> = tv_manager.list_tv_ids().await;
        let manager = Arc::new(tv_manager);

        // Tests use the default poll interval — the streaming-discovery
        // path is exercised through Layer-C end-to-end fixtures that
        // don't depend on a precise iteration period.
        // Tests use the default poll interval — the streaming-
        // discovery and manual-entry paths are exercised through
        // Layer-C fixtures that don't depend on the exact value.
        let poll_interval = tisvlay_core::DiscoveryConfig::default().poll_interval();
        let manager_service =
            ManagerService::new(manager.clone(), registry_for_service, poll_interval);

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

        let poll_loop = tokio::spawn(state_poll_loop(manager.clone()));

        Ok(Self {
            connection,
            manager,
            handle,
            forwarder,
            poll_loop,
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

    /// Build a daemon with a single managed, poll-only mock TV.
    ///
    /// The mock's `subscribe_state` returns `NotSupported` (via the
    /// `disable_subscription` vendor-config knob), so the daemon
    /// exercises its poll/active-verify paths instead of subscription
    /// pushes. See [`mock_tv_config_poll_only`].
    #[instrument]
    pub async fn with_one_poll_only_mock_tv(tv_id: &str) -> anyhow::Result<Self> {
        Self::with_config(mock_tv_config_poll_only(tv_id, true, None)).await
    }

    /// Build a daemon with a single managed, `reachable_in_standby` mock TV
    /// (CEC-style: the connection survives power-off). See
    /// [`mock_tv_config_reachable_in_standby`].
    pub async fn with_one_reachable_in_standby_mock_tv(tv_id: &str) -> anyhow::Result<Self> {
        Self::with_config(mock_tv_config_reachable_in_standby(tv_id, true)).await
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

    /// Path to the config file backing this daemon. Tests can overwrite
    /// this file (via `SystemConfig::save_to`) and then call
    /// `manager().reload_config()` to exercise reload paths.
    pub fn config_path(&self) -> std::path::PathBuf {
        self._config_dir.path().join("config.toml")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.forwarder.abort();
        self.poll_loop.abort();
    }
}
