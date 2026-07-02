//! Backend registry for tisvlay.
//!
//! This crate is the **single composition point** for all vendor implementations.
//! Only this crate imports vendor crates like `tisvlay-lg`. All other binaries
//! (daemon, CLI, GUI, session) import this crate and remain vendor-agnostic.
//!
//! # Adding a new vendor
//!
//! 1. Create a new crate implementing `tisvlay-core` traits (e.g., `tisvlay-samsung`)
//! 2. Add it as a dependency in this crate's `Cargo.toml`
//! 3. Register it in `create_registry()` below
//!
//! No changes needed to daemon, CLI, or other binaries.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(test)]
use tisvlay_core::Capabilities;
use tisvlay_core::{
    Credentials, DiscoveredTv, PairingSession, TvBackend, TvConfig, TvConnection, TvConnector,
};

// Re-export under the mock feature so consumers (e.g., the daemon
// test harness) can name `MockHandle` without taking a direct dep on
// the `tisvlay-mock` crate.
#[cfg(feature = "mock")]
pub use tisvlay_mock::MockHandle;

/// Type-erased discovery function.
///
/// Since `TvDiscovery` uses native `async fn` (not `#[async_trait]`) and is
/// therefore not object-safe, we store discovery implementations as boxed
/// closures that return boxed futures.
type DiscoverFn = dyn Fn(
        std::time::Duration,
    )
        -> Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<Vec<DiscoveredTv>>> + Send>>
    + Send
    + Sync;

/// A registered vendor backend.
///
/// Holds the vendor's [`TvBackend`] trait object as the single source of
/// truth for per-vendor metadata (id, name, capabilities, reliable-tier
/// tuning, future fields). The trait object is `Arc<dyn TvBackend>`
/// rather than the denormalized fields the registry previously stored —
/// every new metadata field is now a single trait method addition,
/// avoiding the trait/struct/register triple-touch that mirroring
/// requires.
pub struct RegisteredBackend {
    /// Vendor metadata. Use `backend.vendor_id()`, `backend.vendor_name()`,
    /// `backend.capabilities()`, etc.
    pub backend: Arc<dyn TvBackend>,
    /// Factory for creating connections.
    pub connector: Arc<dyn TvConnector>,
    /// Discovery function (if supported).
    ///
    /// Stored as a type-erased closure since `TvDiscovery` uses native `async fn`
    /// and is not object-safe.
    pub(crate) discover_fn: Option<Arc<DiscoverFn>>,
}

impl std::fmt::Debug for RegisteredBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredBackend")
            .field("vendor_id", &self.backend.vendor_id())
            .field("vendor_name", &self.backend.vendor_name())
            .field("capabilities", &self.backend.capabilities())
            .field("has_discover_fn", &self.discover_fn.is_some())
            .finish_non_exhaustive()
    }
}

/// Registry of vendor backends.
///
/// This registry holds all available vendor backends and provides methods
/// to create connections based on vendor ID. All transport details are
/// handled by the connectors — the registry is fully transport-agnostic.
#[derive(Debug)]
pub struct BackendRegistry {
    backends: HashMap<&'static str, RegisteredBackend>,
}

impl BackendRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            backends: HashMap::new(),
        }
    }

    /// Register a vendor backend.
    ///
    /// `backend` is the single source of truth for vendor metadata —
    /// `vendor_id`, `vendor_name`, `capabilities`, the reliable-tier
    /// tuning knobs, and any future per-vendor static facts. The
    /// registry stores the trait object directly so adding a new field
    /// to [`TvBackend`] surfaces it to every caller without changes to
    /// this function or `RegisteredBackend`.
    pub fn register(
        &mut self,
        backend: Arc<dyn TvBackend>,
        connector: Arc<dyn TvConnector>,
        discover_fn: Option<Arc<DiscoverFn>>,
    ) {
        let vendor_id = backend.vendor_id();
        self.backends.insert(
            vendor_id,
            RegisteredBackend {
                backend,
                connector,
                discover_fn,
            },
        );
    }

    /// Get a backend by vendor ID.
    pub fn get(&self, vendor_id: &str) -> Option<&RegisteredBackend> {
        self.backends.get(vendor_id)
    }

    /// Get the connector for a vendor.
    pub fn get_connector(&self, vendor_id: &str) -> tisvlay_core::Result<&dyn TvConnector> {
        let backend = self
            .get(vendor_id)
            .ok_or_else(|| tisvlay_core::Error::UnknownVendor(vendor_id.to_string()))?;
        Ok(backend.connector.as_ref())
    }

    /// Get all registered backends.
    pub fn all(&self) -> impl Iterator<Item = &RegisteredBackend> {
        self.backends.values()
    }

    /// Get all supported vendor IDs and names.
    pub fn supported_vendors(&self) -> Vec<(&'static str, &'static str)> {
        self.backends
            .values()
            .map(|b| (b.backend.vendor_id(), b.backend.vendor_name()))
            .collect()
    }

    // =========================================================================
    // TvConfig-based convenience methods (vendor/transport-agnostic)
    // =========================================================================

    /// Connect to a TV using its config.
    ///
    /// Looks up the vendor, extracts the vendor config section, and delegates
    /// to the appropriate connector. The caller never needs to know vendor
    /// or transport details.
    pub async fn connect_tv(
        &self,
        tv_config: &TvConfig,
    ) -> tisvlay_core::Result<Box<dyn TvConnection>> {
        let connector = self.get_connector(&tv_config.vendor)?;
        let vendor_config = tv_config.get_raw_vendor_value().ok_or_else(|| {
            tisvlay_core::Error::backend(
                &tv_config.vendor,
                format!("No vendor config for TV '{}'", tv_config.uuid),
            )
        })?;
        connector.connect(vendor_config).await
    }

    /// User-editable vendor-config rows for the GUI's TVs-pane
    /// CONFIG group. Wraps [`TvConnector::display_config`] —
    /// returns an empty vec for unknown vendors or backends with
    /// no `UserInput` schema fields.
    pub fn display_config(&self, tv_config: &TvConfig) -> Vec<(String, String)> {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| {
                tv_config
                    .get_raw_vendor_value()
                    .map(|v| c.display_config(v))
            })
            .unwrap_or_default()
    }

    /// Read-only diagnostic vendor rows for the GUI's TVs-pane
    /// INFO group. Wraps [`TvConnector::display_diagnostics`] —
    /// returns an empty vec for unknown vendors or backends with
    /// no `Diagnostic` schema fields.
    pub fn display_diagnostics(&self, tv_config: &TvConfig) -> Vec<(String, String)> {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| {
                tv_config
                    .get_raw_vendor_value()
                    .map(|v| c.display_diagnostics(v))
            })
            .unwrap_or_default()
    }

    /// Backend's primary descriptor — the value of the first
    /// `UserInput`-visibility entry from the vendor config.
    /// Convention: backends order their `config_schema` so the
    /// most informative editable field (LG: ip; future Bluetooth:
    /// MAC; USB-CEC: serial port) comes first. Used by
    /// daemon-internal log formatters and route resolution where
    /// a single transport-appropriate string is needed.
    ///
    /// Returns `None` when the backend declines to surface any
    /// editable info (mock without `UserInput` fields, future
    /// minimal backends).
    pub fn primary_descriptor(&self, tv_config: &TvConfig) -> Option<String> {
        self.display_config(tv_config)
            .into_iter()
            .next()
            .map(|(_, value)| value)
    }

    /// Get the transport type for a TV's backend.
    ///
    /// Returns `Transport::Network` if the vendor is unknown (safe default —
    /// network events will still disconnect it, which is correct for unknown devices).
    pub fn transport_for(&self, tv_config: &TvConfig) -> tisvlay_core::Transport {
        self.get_connector(&tv_config.vendor)
            .map(|c| c.transport())
            .unwrap_or(tisvlay_core::Transport::Network)
    }

    /// Whether a TV's backend connections survive client source IP changes.
    ///
    /// Returns `false` for unknown vendors (safe default — reconnect on NIC change).
    pub fn survives_source_ip_change(&self, tv_config: &TvConfig) -> bool {
        self.get_connector(&tv_config.vendor)
            .map(|c| c.survives_source_ip_change())
            .unwrap_or(false)
    }

    /// Check if a TV's transport supports waking.
    pub fn supports_wake(&self, tv_config: &TvConfig) -> bool {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| tv_config.get_raw_vendor_value().map(|v| c.supports_wake(v)))
            .unwrap_or(false)
    }

    /// Whether this TV's backend keeps a live connection while the TV is in
    /// standby (CEC's bus stays reachable; a network socket does not). The
    /// daemon uses this to keep the connection — and `Connected` — across a
    /// power-off, treating power and connection as separate axes.
    pub fn reachable_in_standby(&self, tv_config: &TvConfig) -> bool {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| {
                tv_config
                    .get_raw_vendor_value()
                    .map(|v| c.reachable_in_standby(v))
            })
            .unwrap_or(false)
    }

    /// Whether this TV's backend fulfils screen-blank actions by powering the
    /// device (emulates `SCREEN` via `POWER`). The daemon uses this to
    /// advertise `SCREEN` and dispatch `screen_off`/`screen_on` as
    /// `power_off`/`power_on` for a device with no discrete screen-blank
    /// (HDMI-CEC).
    pub fn emulate_screen_via_power(&self, tv_config: &TvConfig) -> bool {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| {
                tv_config
                    .get_raw_vendor_value()
                    .map(|v| c.emulate_screen_via_power(v))
            })
            .unwrap_or(false)
    }

    /// Wake a TV from standby.
    pub async fn wake_tv(&self, tv_config: &TvConfig) -> tisvlay_core::Result<()> {
        let connector = self.get_connector(&tv_config.vendor)?;
        let vendor_config = tv_config.get_raw_vendor_value().ok_or_else(|| {
            tisvlay_core::Error::backend(
                &tv_config.vendor,
                format!("No vendor config for TV '{}'", tv_config.uuid),
            )
        })?;
        connector.wake(vendor_config).await
    }

    /// Wake a TV and establish a connection.
    ///
    /// The backend owns all transport-specific retry logic (timing, retry
    /// count, wake packet resends, etc.).
    pub async fn wake_and_connect_tv(
        &self,
        tv_config: &TvConfig,
    ) -> tisvlay_core::Result<Box<dyn TvConnection>> {
        let connector = self.get_connector(&tv_config.vendor)?;
        let vendor_config = tv_config.get_raw_vendor_value().ok_or_else(|| {
            tisvlay_core::Error::backend(
                &tv_config.vendor,
                format!("No vendor config for TV '{}'", tv_config.uuid),
            )
        })?;
        connector.wake_and_connect(vendor_config).await
    }

    /// Start pairing with a new device.
    pub fn start_pairing(
        &self,
        vendor: &str,
        vendor_config: &toml::Value,
    ) -> tisvlay_core::Result<PairingSession> {
        let connector = self.get_connector(vendor)?;
        connector.start_pairing(vendor_config)
    }

    /// Build vendor config from pairing results.
    pub fn build_config_from_pairing(
        &self,
        vendor: &str,
        pairing_params: &toml::Value,
        credentials: &Credentials,
    ) -> tisvlay_core::Result<toml::Value> {
        let connector = self.get_connector(vendor)?;
        connector.build_config_from_pairing(pairing_params, credentials)
    }

    /// Discover TVs for a specific vendor.
    ///
    /// Returns an empty list if the vendor has no discovery implementation
    /// registered. Unlike [`discover_all`](Self::discover_all), this method
    /// is available unconditionally — it's used by tests that target a
    /// single vendor (e.g., the mock) regardless of whether real-network
    /// discovery is compiled in.
    pub async fn discover_tvs(
        &self,
        vendor: &str,
        timeout: std::time::Duration,
    ) -> tisvlay_core::Result<Vec<tisvlay_core::DiscoveredTv>> {
        let backend = self
            .get(vendor)
            .ok_or_else(|| tisvlay_core::Error::UnknownVendor(vendor.to_string()))?;
        match &backend.discover_fn {
            Some(discover_fn) => discover_fn(timeout).await,
            None => Ok(Vec::new()),
        }
    }

    /// Discover TVs using all backends that support discovery.
    #[cfg(feature = "discovery")]
    pub async fn discover_all(
        &self,
        timeout: std::time::Duration,
    ) -> tisvlay_core::Result<Vec<tisvlay_core::DiscoveredTv>> {
        let mut all_tvs = Vec::new();

        for backend in self.backends.values() {
            if let Some(discover_fn) = &backend.discover_fn {
                match discover_fn(timeout).await {
                    Ok(tvs) => all_tvs.extend(tvs),
                    Err(e) => {
                        tracing::warn!(
                            vendor = backend.backend.vendor_id(),
                            error = %e,
                            "Discovery failed for backend"
                        );
                    }
                }
            }
        }

        Ok(all_tvs)
    }
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register the LG WebOS backend on an existing registry.
///
/// Factored out so both `create_registry()` and `create_test_registry()`
/// register LG the same way — there's only one way to register LG and
/// no reason to duplicate the wiring. `discovery` carries the daemon's
/// discovery settings (bind port, …); the test registry passes a
/// default config since tests don't exercise the firewall path.
#[cfg_attr(
    not(feature = "discovery"),
    expect(
        unused_variables,
        reason = "discovery config is only consulted when the discovery feature is enabled"
    )
)]
fn register_lg(registry: &mut BackendRegistry, discovery: &tisvlay_core::DiscoveryConfig) {
    let lg_connector = Arc::new(tisvlay_lg::LgConnector::new());

    #[cfg(feature = "discovery")]
    let lg_discover_fn: Option<Arc<DiscoverFn>> = {
        use tisvlay_core::TvDiscovery;
        // Resolve the effective source port (config value or the 1901
        // default) so the shared SSDP socket always binds a known,
        // firewall-targetable port. A configured 0 still means ephemeral.
        let lg_discovery = Arc::new(tisvlay_lg::LgDiscovery::new(discovery.bind_port()));
        Some(Arc::new(move |timeout| {
            let lg = Arc::clone(&lg_discovery);
            Box::pin(async move { lg.discover(timeout).await })
        }))
    };

    #[cfg(not(feature = "discovery"))]
    let lg_discover_fn: Option<Arc<DiscoverFn>> = None;

    registry.register(
        Arc::new(tisvlay_lg::LgBackend::new()),
        lg_connector,
        lg_discover_fn,
    );
}

/// Register the HDMI-CEC backend on an existing registry.
///
/// CEC discovery is enumerating the host's `/dev/cecN` nodes — no network
/// or SSDP — so it's wired unconditionally (not behind the `discovery`
/// feature, which gates only the network backends' SSDP). Shared between
/// the production and test registries so both wire CEC identically.
fn register_cec(registry: &mut BackendRegistry) {
    use tisvlay_core::TvDiscovery;
    let discovery = Arc::new(tisvlay_cec::CecDiscovery::new());
    let discover_fn: Arc<DiscoverFn> = Arc::new(move |timeout| {
        let discovery = Arc::clone(&discovery);
        Box::pin(async move { discovery.discover(timeout).await })
    });
    registry.register(
        Arc::new(tisvlay_cec::CecBackend::new()),
        Arc::new(tisvlay_cec::CecConnector::new()),
        Some(discover_fn),
    );
}

/// Register the mock backend (for production use) on an existing
/// registry. The mock's state is private and inaccessible, so
/// discovery returns an empty list.
#[cfg(feature = "mock")]
fn register_mock_production(registry: &mut BackendRegistry) {
    use tisvlay_core::TvDiscovery;
    let (connector, discovery) = tisvlay_mock::new_production_parts();
    let connector = Arc::new(connector);
    let discovery = Arc::new(discovery);
    let discover_fn: Arc<DiscoverFn> = Arc::new(move |timeout| {
        let discovery = Arc::clone(&discovery);
        Box::pin(async move { discovery.discover(timeout).await })
    });
    registry.register(
        Arc::new(tisvlay_mock::MockBackend::new()),
        connector,
        Some(discover_fn),
    );
}

/// Create a registry with all supported backends registered.
///
/// This is the main entry point for creating a backend registry in
/// production code. All vendor backends are registered here.
/// `discovery` carries daemon-level discovery settings (currently
/// just the SSDP bind port) that the LG backend (and any future
/// vendor that does SSDP) plumbs through.
pub fn create_registry(discovery: &tisvlay_core::DiscoveryConfig) -> BackendRegistry {
    let mut registry = BackendRegistry::new();

    register_lg(&mut registry, discovery);
    register_cec(&mut registry);

    #[cfg(feature = "mock")]
    register_mock_production(&mut registry);

    // =========================================================================
    // Future: Register Samsung backend
    // =========================================================================
    // let samsung_backend = Arc::new(tisvlay_samsung::SamsungBackend::new());
    // let samsung_connector = Arc::new(tisvlay_samsung::SamsungConnector::new());
    // registry.register(samsung_backend, samsung_connector, None);

    registry
}

/// Create a registry wired up for tests, returning a
/// [`tisvlay_mock::MockHandle`] alongside the registry.
///
/// The returned handle shares state with the mock backend in the
/// registry, so tests can drive discovery and pairing outcomes
/// out-of-band. Each call constructs a fresh state; tests never
/// share state with each other.
///
/// LG is registered as well so tests that exercise multi-vendor
/// lookup paths work correctly. The LG connector is not wired to
/// any real TVs — calling it will fail, which is the intended
/// behavior for tests.
#[cfg(feature = "mock")]
pub fn create_test_registry() -> (BackendRegistry, tisvlay_mock::MockHandle) {
    use tisvlay_core::TvDiscovery;

    let mut registry = BackendRegistry::new();

    register_lg(&mut registry, &tisvlay_core::DiscoveryConfig::default());
    register_cec(&mut registry);

    let (connector, discovery, handle) = tisvlay_mock::new_test_parts();
    let connector = Arc::new(connector);
    let discovery = Arc::new(discovery);
    let discover_fn: Arc<DiscoverFn> = Arc::new(move |timeout| {
        let discovery = Arc::clone(&discovery);
        Box::pin(async move { discovery.discover(timeout).await })
    });
    registry.register(
        Arc::new(tisvlay_mock::MockBackend::new()),
        connector,
        Some(discover_fn),
    );

    (registry, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = create_registry(&tisvlay_core::DiscoveryConfig::default());
        assert!(registry.get("lg").is_some());
        assert!(registry.get("samsung").is_none());
    }

    #[test]
    fn test_supported_vendors() {
        let registry = create_registry(&tisvlay_core::DiscoveryConfig::default());
        let vendors = registry.supported_vendors();
        assert!(vendors.iter().any(|(id, _)| *id == "lg"));
    }

    #[test]
    fn test_lg_backend_registered() {
        let registry = create_registry(&tisvlay_core::DiscoveryConfig::default());
        let lg = registry.get("lg").expect("LG backend should be registered");
        assert_eq!(lg.backend.vendor_id(), "lg");
        assert_eq!(lg.backend.vendor_name(), "LG WebOS");
        assert!(lg.backend.capabilities().contains(Capabilities::POWER));
        assert!(lg.backend.capabilities().contains(Capabilities::SCREEN));
    }
}
