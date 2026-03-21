//! D-Bus Manager interface implementation.
//!
//! Implements `org.tisvlay.Manager1` for TV discovery and management.

use crate::dbus::{register_tv_interfaces, unregister_tv_interfaces, DaemonError};
use crate::registry::BackendRegistry;
use crate::tv_manager::TvManager;
use std::sync::Arc;
use tisvlay_core::dbus_paths::tv_path;

use tracing::{debug, info, warn};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;

/// D-Bus Manager service.
///
/// Provides TV management operations at `/org/tisvlay/Manager`.
pub struct ManagerService {
    manager: Arc<TvManager>,
    registry: Arc<BackendRegistry>,
    connection: zbus::Connection,
}

impl ManagerService {
    /// Create a new manager service.
    pub fn new(
        manager: Arc<TvManager>,
        registry: Arc<BackendRegistry>,
        connection: zbus::Connection,
    ) -> Self {
        Self {
            manager,
            registry,
            connection,
        }
    }
}

#[interface(name = "org.tisvlay.Manager1")]
impl ManagerService {
    // -------------------------------------------------------------------------
    // Discovery
    // -------------------------------------------------------------------------

    /// Discover TVs on the network.
    ///
    /// Returns: Array of (id, name, address, hardware_id, vendor) tuples.
    #[cfg(feature = "discovery")]
    async fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> Result<Vec<(String, String, String, String, String)>, DaemonError> {
        info!(timeout_ms = timeout_ms, "D-Bus: DiscoverTvs");

        let tvs = self
            .registry
            .discover_all(std::time::Duration::from_millis(timeout_ms as u64))
            .await
            .map_err(DaemonError::from)?;

        Ok(tvs
            .into_iter()
            .map(|tv| (tv.id, tv.name, tv.address, tv.hardware_id, tv.vendor))
            .collect())
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// Get all TV object paths.
    async fn get_tvs(&self) -> Vec<ObjectPath<'static>> {
        debug!("D-Bus: GetTvs");
        let manager = &self.manager;
        manager
            .list_tv_ids()
            .await
            .into_iter()
            .map(|id| ObjectPath::try_from(tv_path(&id)).unwrap())
            .collect()
    }

    /// Get supported vendor backends.
    ///
    /// Returns: Array of (vendor_id, vendor_name) tuples.
    fn get_supported_vendors(&self) -> Vec<(String, String)> {
        debug!("D-Bus: GetSupportedVendors");
        self.registry
            .supported_vendors()
            .into_iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect()
    }

    /// Add a new TV.
    ///
    /// Starts pairing with the TV using the provided vendor config JSON.
    /// The config_json should contain vendor-specific connection parameters
    /// (e.g., `{"ip": "192.168.1.100", "mac": "AA:BB:CC:DD:EE:FF"}` for LG).
    ///
    /// Returns: Object path for the new TV.
    async fn add_tv(
        &self,
        name: &str,
        vendor: &str,
        config_json: &str,
    ) -> Result<ObjectPath<'static>, DaemonError> {
        info!(name = %name, vendor = %vendor, "D-Bus: AddTv");

        // Verify vendor is supported
        if self.registry.get(vendor).is_none() {
            return Err(DaemonError::UnknownVendor(vendor.to_string()));
        }

        // Parse JSON config to serde_json::Value, then convert to toml::Value
        let json_value: serde_json::Value = serde_json::from_str(config_json)
            .map_err(|e| DaemonError::OperationFailed(format!("Invalid config JSON: {}", e)))?;

        let vendor_config = json_to_toml(&json_value).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to convert config: {}", e))
        })?;

        let manager = &self.manager;
        let tv_id = manager
            .pair_tv(name, vendor, vendor_config, None)
            .await
            .map_err(DaemonError::from)?;

        // Register D-Bus interfaces for the newly added TV.
        let path = tv_path(&tv_id);
        if let Err(e) =
            register_tv_interfaces(&self.connection, &path, tv_id.clone(), manager.clone()).await
        {
            warn!(tv_id = %tv_id, error = %e, "Failed to register D-Bus interfaces for new TV");
        }

        Ok(ObjectPath::try_from(path).unwrap())
    }

    /// Remove a TV.
    async fn remove_tv(&self, tv_path: ObjectPath<'_>) -> Result<(), DaemonError> {
        // Extract TV ID from path
        let path_str = tv_path.as_str();
        let tv_id = path_str
            .strip_prefix("/org/tisvlay/Tv/")
            .ok_or_else(|| DaemonError::TvNotFound(format!("Invalid path: {}", path_str)))?;

        info!(tv_id = %tv_id, "D-Bus: RemoveTv");

        let manager = &self.manager;
        manager.remove_tv(tv_id).await.map_err(DaemonError::from)?;

        // Unregister D-Bus interfaces for the removed TV.
        if let Err(e) = unregister_tv_interfaces(&self.connection, path_str).await {
            warn!(tv_id = %tv_id, error = %e, "Failed to unregister D-Bus interfaces for TV");
        }

        Ok(())
    }

    /// Reload configuration from disk.
    async fn reload_config(&self) -> Result<(), DaemonError> {
        info!("D-Bus: ReloadConfig");
        let manager = &self.manager;
        manager.reload_config().await.map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // System Config Management
    // -------------------------------------------------------------------------

    /// Set a daemon configuration option.
    ///
    /// Supported keys: `wake_on_boot`, `wake_delay_ms`, `on_shutdown`,
    /// `on_no_active_session`.
    async fn set_daemon_option(&self, key: &str, value: &str) -> Result<(), DaemonError> {
        info!(key = %key, value = %value, "D-Bus: SetDaemonOption");
        self.manager
            .set_daemon_option(key, value)
            .map_err(DaemonError::from)
    }

    /// Set a system-level TV option.
    ///
    /// Supported keys: `hdmi_input`, `only_when_active`, `name`.
    async fn set_tv_option(
        &self,
        tv_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, key = %key, value = %value, "D-Bus: SetTvOption");
        self.manager
            .set_tv_option(tv_id, key, value)
            .await
            .map_err(DaemonError::from)
    }

    /// Get the system default profile as JSON.
    ///
    /// Returns the `[profiles.default]` from the system config.
    /// Used by session services to inherit defaults when users
    /// have no config file.
    async fn get_system_profile(&self) -> Result<String, DaemonError> {
        debug!("D-Bus: GetSystemProfile");
        let profile = self.manager.get_system_default_profile().await;
        serde_json::to_string(&profile).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to serialize profile: {}", e))
        })
    }

    // -------------------------------------------------------------------------
    // Signals
    // -------------------------------------------------------------------------

    /// Signal emitted when a TV is added.
    #[zbus(signal)]
    pub async fn tv_added(emitter: &SignalEmitter<'_>, tv_path: ObjectPath<'_>)
        -> zbus::Result<()>;

    /// Signal emitted when a TV is removed.
    #[zbus(signal)]
    pub async fn tv_removed(
        emitter: &SignalEmitter<'_>,
        tv_path: ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

/// Convert a serde_json::Value to a toml::Value.
fn json_to_toml(json: &serde_json::Value) -> Result<toml::Value, String> {
    match json {
        serde_json::Value::Null => {
            Err("JSON null values are not supported in TOML config".to_string())
        }
        serde_json::Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err("Unsupported number type".to_string())
            }
        }
        serde_json::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let values: Result<Vec<toml::Value>, String> = arr.iter().map(json_to_toml).collect();
            Ok(toml::Value::Array(values?))
        }
        serde_json::Value::Object(obj) => {
            let mut table = toml::map::Map::new();
            for (key, value) in obj {
                table.insert(key.clone(), json_to_toml(value)?);
            }
            Ok(toml::Value::Table(table))
        }
    }
}
