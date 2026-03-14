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
use std::sync::Arc;
use tisvlay_core::{Capabilities, Credentials, PairingSession, TvConfig, TvConnection, TvConnector, TvDiscovery};

// Re-export core types for convenience
pub use tisvlay_core::{
    Capabilities as TvCapabilities, Config, DiscoveredTv, Error, InputInfo, PairingSession as PairSession,
    PairingState, PowerState, ProfileConfig, Result, SessionAction, TvConfig as TvCfg, TvState,
};

/// A registered vendor backend.
pub struct RegisteredBackend {
    /// Vendor identifier (e.g., "lg", "samsung").
    pub vendor_id: &'static str,
    /// Human-readable vendor name (e.g., "LG WebOS").
    pub vendor_name: &'static str,
    /// Capabilities supported by this backend.
    pub capabilities: Capabilities,
    /// Factory for creating connections.
    pub connector: Arc<dyn TvConnector>,
    /// Discovery implementation (if supported).
    pub discovery: Option<Arc<dyn TvDiscovery>>,
}

/// Registry of vendor backends.
///
/// This registry holds all available vendor backends and provides methods
/// to create connections based on vendor ID. All transport details are
/// handled by the connectors — the registry is fully transport-agnostic.
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
    pub fn register(
        &mut self,
        vendor_id: &'static str,
        vendor_name: &'static str,
        capabilities: Capabilities,
        connector: Arc<dyn TvConnector>,
        discovery: Option<Arc<dyn TvDiscovery>>,
    ) {
        self.backends.insert(
            vendor_id,
            RegisteredBackend {
                vendor_id,
                vendor_name,
                capabilities,
                connector,
                discovery,
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
            .map(|b| (b.vendor_id, b.vendor_name))
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
        let vendor_config = tv_config
            .get_raw_vendor_value()
            .ok_or_else(|| {
                tisvlay_core::Error::backend(
                    &tv_config.vendor,
                    format!("No vendor config for TV '{}'", tv_config.id),
                )
            })?;
        connector.connect(vendor_config).await
    }

    /// Get a human-readable display address for a TV.
    pub fn display_address(&self, tv_config: &TvConfig) -> String {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| tv_config.get_raw_vendor_value().map(|v| c.display_address(v)))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Check if a TV's transport supports waking.
    pub fn supports_wake(&self, tv_config: &TvConfig) -> bool {
        self.get_connector(&tv_config.vendor)
            .ok()
            .and_then(|c| tv_config.get_raw_vendor_value().map(|v| c.supports_wake(v)))
            .unwrap_or(false)
    }

    /// Wake a TV from standby.
    pub async fn wake_tv(&self, tv_config: &TvConfig) -> tisvlay_core::Result<()> {
        let connector = self.get_connector(&tv_config.vendor)?;
        let vendor_config = tv_config
            .get_raw_vendor_value()
            .ok_or_else(|| {
                tisvlay_core::Error::backend(
                    &tv_config.vendor,
                    format!("No vendor config for TV '{}'", tv_config.id),
                )
            })?;
        connector.wake(vendor_config).await
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

    /// Discover TVs using all backends that support discovery.
    #[cfg(feature = "discovery")]
    pub async fn discover_all(
        &self,
        timeout: std::time::Duration,
    ) -> tisvlay_core::Result<Vec<tisvlay_core::DiscoveredTv>> {
        let mut all_tvs = Vec::new();

        for backend in self.backends.values() {
            if let Some(discovery) = &backend.discovery {
                match discovery.discover(timeout).await {
                    Ok(tvs) => all_tvs.extend(tvs),
                    Err(e) => {
                        tracing::warn!(
                            vendor = backend.vendor_id,
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

/// Create a registry with all supported backends registered.
///
/// This is the main entry point for creating a backend registry.
/// All vendor backends are registered here.
pub fn create_registry() -> BackendRegistry {
    let mut registry = BackendRegistry::new();

    // =========================================================================
    // Register LG WebOS backend
    // =========================================================================
    let lg_connector = Arc::new(tisvlay_lg::LgConnector::new());

    #[cfg(feature = "discovery")]
    let lg_discovery: Option<Arc<dyn TvDiscovery>> =
        Some(Arc::new(tisvlay_lg::LgDiscovery::new()));

    #[cfg(not(feature = "discovery"))]
    let lg_discovery: Option<Arc<dyn TvDiscovery>> = None;

    registry.register(
        tisvlay_lg::VENDOR_ID,
        tisvlay_lg::VENDOR_NAME,
        Capabilities::ALL,
        lg_connector,
        lg_discovery,
    );

    // =========================================================================
    // Future: Register Samsung backend
    // =========================================================================
    // let samsung_connector = Arc::new(tisvlay_samsung::SamsungConnector::new());
    // registry.register(
    //     tisvlay_samsung::VENDOR_ID,
    //     tisvlay_samsung::VENDOR_NAME,
    //     Capabilities::SAMSUNG_ALL,
    //     samsung_connector,
    //     None,
    // );

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = create_registry();
        assert!(registry.get("lg").is_some());
        assert!(registry.get("samsung").is_none());
    }

    #[test]
    fn test_supported_vendors() {
        let registry = create_registry();
        let vendors = registry.supported_vendors();
        assert!(vendors.iter().any(|(id, _)| *id == "lg"));
    }

    #[test]
    fn test_lg_backend_registered() {
        let registry = create_registry();
        let lg = registry.get("lg").expect("LG backend should be registered");
        assert_eq!(lg.vendor_id, "lg");
        assert_eq!(lg.vendor_name, "LG WebOS");
        assert!(lg.capabilities.contains(Capabilities::POWER));
        assert!(lg.capabilities.contains(Capabilities::SCREEN));
    }
}
