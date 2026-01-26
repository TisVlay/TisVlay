//! D-Bus Manager interface implementation.
//!
//! Implements `org.tisvlay.Manager1` for TV discovery and management.

use crate::dbus::{tv_path, DaemonError};
use crate::registry::BackendRegistry;
use crate::tv_manager::TvManager;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;

/// D-Bus Manager service.
///
/// Provides TV management operations at `/org/tisvlay/Manager`.
pub struct ManagerService {
    manager: Arc<RwLock<TvManager>>,
    registry: Arc<BackendRegistry>,
}

impl ManagerService {
    /// Create a new manager service.
    pub fn new(manager: Arc<RwLock<TvManager>>, registry: Arc<BackendRegistry>) -> Self {
        Self { manager, registry }
    }
}

#[interface(name = "org.tisvlay.Manager1")]
impl ManagerService {
    // -------------------------------------------------------------------------
    // Discovery
    // -------------------------------------------------------------------------

    /// Discover TVs on the network.
    ///
    /// Returns: Array of (id, name, ip, mac, vendor) tuples.
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
            .map(|tv| (tv.id, tv.name, tv.ip, tv.mac, tv.vendor))
            .collect())
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// Get all TV object paths.
    async fn get_tvs(&self) -> Vec<ObjectPath<'static>> {
        debug!("D-Bus: GetTvs");
        let manager = self.manager.read().await;
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
    /// Starts pairing with the TV at the given IP address.
    ///
    /// Returns: Object path for the new TV.
    async fn add_tv(
        &self,
        ip: &str,
        name: &str,
        vendor: &str,
        mac: &str,
    ) -> Result<ObjectPath<'static>, DaemonError> {
        info!(ip = %ip, name = %name, vendor = %vendor, "D-Bus: AddTv");

        // Verify vendor is supported
        if self.registry.get(vendor).is_none() {
            return Err(DaemonError::UnknownVendor(vendor.to_string()));
        }

        let mac_opt = if mac.is_empty() { None } else { Some(mac) };

        let manager = self.manager.read().await;
        let tv_id = manager
            .pair_tv(ip, name, vendor, mac_opt, None)
            .await
            .map_err(DaemonError::from)?;

        Ok(ObjectPath::try_from(tv_path(&tv_id)).unwrap())
    }

    /// Remove a TV.
    async fn remove_tv(&self, tv_path: ObjectPath<'_>) -> Result<(), DaemonError> {
        // Extract TV ID from path
        let path_str = tv_path.as_str();
        let tv_id = path_str
            .strip_prefix("/org/tisvlay/Tv/")
            .ok_or_else(|| DaemonError::TvNotFound(format!("Invalid path: {}", path_str)))?;

        info!(tv_id = %tv_id, "D-Bus: RemoveTv");

        let manager = self.manager.read().await;
        manager.remove_tv(tv_id).await.map_err(DaemonError::from)
    }

    /// Reload configuration from disk.
    async fn reload_config(&self) -> Result<(), DaemonError> {
        info!("D-Bus: ReloadConfig");
        let manager = self.manager.read().await;
        manager.reload_config().await.map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // Signals
    // -------------------------------------------------------------------------

    /// Signal emitted when a TV is added.
    #[zbus(signal)]
    pub async fn tv_added(
        emitter: &SignalEmitter<'_>,
        tv_path: ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Signal emitted when a TV is removed.
    #[zbus(signal)]
    pub async fn tv_removed(
        emitter: &SignalEmitter<'_>,
        tv_path: ObjectPath<'_>,
    ) -> zbus::Result<()>;
}
