//! Backend registry for managing TV vendor backends.
//!
//! The registry holds vendor backends (LG, Samsung, etc.) and provides
//! methods to create connections based on vendor ID.

use std::collections::HashMap;
use std::sync::Arc;
use tisvlay_core::{Capabilities, TvConnection, TvConnector, TvDiscovery};

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

    /// Connect to a TV using the appropriate backend.
    pub async fn connect(
        &self,
        vendor_id: &str,
        ip: &str,
        mac: Option<&str>,
        vendor_config: &(dyn std::any::Any + Send + Sync),
    ) -> tisvlay_core::Result<Box<dyn TvConnection>> {
        let backend = self.get(vendor_id).ok_or_else(|| {
            tisvlay_core::Error::UnknownVendor(vendor_id.to_string())
        })?;

        backend.connector.connect(ip, mac, vendor_config).await
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

/// Create a registry with the default backends registered.
pub fn create_default_registry() -> BackendRegistry {
    let mut registry = BackendRegistry::new();

    // Register LG backend
    let lg_connector = Arc::new(tisvlay_lg::LgConnector::new());

    #[cfg(feature = "discovery")]
    let lg_discovery: Option<Arc<dyn TvDiscovery>> = Some(Arc::new(tisvlay_lg::LgDiscovery::new()));

    #[cfg(not(feature = "discovery"))]
    let lg_discovery: Option<Arc<dyn TvDiscovery>> = None;

    registry.register(
        tisvlay_lg::VENDOR_ID,
        tisvlay_lg::VENDOR_NAME,
        Capabilities::LG_ALL,
        lg_connector,
        lg_discovery,
    );

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = create_default_registry();
        assert!(registry.get("lg").is_some());
        assert!(registry.get("samsung").is_none());
    }

    #[test]
    fn test_supported_vendors() {
        let registry = create_default_registry();
        let vendors = registry.supported_vendors();
        assert!(vendors.iter().any(|(id, _)| *id == "lg"));
    }
}
