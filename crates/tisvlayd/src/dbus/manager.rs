//! D-Bus Manager interface implementation.
//!
//! Implements `org.tisvlay.Manager1` for TV discovery and management.

use crate::dbus::{DaemonError, register_tv_interfaces, unregister_tv_interfaces};
use crate::registry::BackendRegistry;
use crate::tv_manager::TvManager;
use std::collections::HashMap;
use std::sync::Arc;
use tisvlay_core::dbus_paths::tv_path;

use tracing::{debug, info, warn};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;

/// D-Bus Manager service.
///
/// Provides TV management operations at `/org/tisvlay/Manager`.
#[derive(Debug)]
pub struct ManagerService {
    manager: Arc<TvManager>,
    registry: Arc<BackendRegistry>,
}

impl ManagerService {
    /// Create a new manager service.
    pub fn new(manager: Arc<TvManager>, registry: Arc<BackendRegistry>) -> Self {
        Self { manager, registry }
    }
}

#[interface(name = "org.tisvlay.Manager1")]
impl ManagerService {
    // -------------------------------------------------------------------------
    // Discovery
    // -------------------------------------------------------------------------

    /// Discover available TVs using all registered backends.
    ///
    /// Returns: Array of `(id, name, address, (manufacturer, model, serial),
    /// vendor, vendor_data)` tuples. The nested `(sss)` carries the
    /// discovered hardware identity with empty strings for unknown fields.
    /// The trailing `a{ss}` is a free-form `key → value` map of
    /// vendor-specific discovery data — currently used to carry the
    /// kernel-resolved MAC (`vendor_data["mac"]`) for IP-addressed
    /// backends, with autofill keys matching each vendor's pairing-form
    /// schema.
    #[cfg(feature = "discovery")]
    async fn discover_tvs(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        timeout_ms: u32,
    ) -> Result<
        Vec<(
            String,
            String,
            String,
            (String, String, String),
            String,
            HashMap<String, String>,
        )>,
        DaemonError,
    > {
        info!(timeout_ms = timeout_ms, "D-Bus: DiscoverTvs");

        let tvs = self
            .registry
            .discover_all(std::time::Duration::from_millis(timeout_ms as u64))
            .await
            .map_err(DaemonError::from)?;

        // Discovery enrichment: for IP-addressed TVs whose vendor backend
        // didn't already provide a MAC, fill `vendor_data["mac"]` from
        // the kernel neighbour table. SSDP responses themselves prime the
        // ARP cache, so a passive lookup is usually enough — see
        // `crate::netlink::arp_lookup`.
        let mut enriched = Vec::with_capacity(tvs.len());
        for mut tv in tvs {
            if !tv.vendor_data.contains_key("mac") {
                if let Ok(ip) = tv.address.parse::<std::net::IpAddr>() {
                    if let Some(mac) = crate::netlink::arp_lookup(ip).await {
                        tv.vendor_data.insert("mac".to_string(), mac.to_string());
                    }
                }
            }
            enriched.push(tv);
        }

        let results = enriched
            .into_iter()
            .map(|tv| {
                let hardware_id = tv.hardware_id.unwrap_or_default().to_dbus_tuple();
                (
                    tv.id,
                    tv.name,
                    tv.address,
                    hardware_id,
                    tv.vendor,
                    tv.vendor_data,
                )
            })
            .collect::<Vec<_>>();

        // Emit DiscoveryFound for each result before returning, so
        // subscribers see the full list whether they wait on the RPC
        // or only listen to the signal stream. Best-effort — the
        // RPC's return value is the authoritative list.
        for tv in &results {
            if let Err(e) = Self::discovery_found(
                &emitter,
                &tv.0,
                &tv.1,
                &tv.2,
                (&tv.3.0, &tv.3.1, &tv.3.2),
                &tv.4,
                tv.5.clone(),
            )
            .await
            {
                tracing::warn!(error = %e, "Failed to emit DiscoveryFound signal");
            }
        }

        Ok(results)
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

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

    /// Get the pairing-form schema for a vendor backend.
    ///
    /// Returns a JSON-serialized `Vec<PairingField>` describing the
    /// fields the GUI's wizard should render. JSON-on-D-Bus follows
    /// the same convention `AddTv`'s `config_json` uses (per
    /// commit `7b7bf81`); the typed Rust shape lives in
    /// `tisvlay_core::pairing_schema`.
    fn get_pairing_schema(&self, vendor: &str) -> Result<String, DaemonError> {
        debug!(vendor = %vendor, "D-Bus: GetPairingSchema");
        let connector = self
            .registry
            .get(vendor)
            .ok_or_else(|| DaemonError::UnknownVendor(vendor.to_string()))?;
        let schema = connector.connector.pairing_schema();
        serde_json::to_string(&schema).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to serialize pairing schema: {}", e))
        })
    }

    /// Add a new TV.
    ///
    /// Starts pairing with the TV using the provided vendor config JSON.
    /// The config_json should contain vendor-specific connection parameters
    /// (e.g., `{"ip": "192.168.1.100", "mac": "AA:BB:CC:DD:EE:FF"}` for LG).
    ///
    /// Returns: (object_path, tv_id) — the TV ID is the canonical
    /// identifier from TvManager, not derived from the D-Bus path.
    async fn add_tv(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        name: &str,
        vendor: &str,
        config_json: &str,
        managed: bool,
    ) -> Result<(ObjectPath<'static>, String), DaemonError> {
        info!(name = %name, vendor = %vendor, managed = %managed, "D-Bus: AddTv");

        // Verify vendor is supported.
        let backend = self
            .registry
            .get(vendor)
            .ok_or_else(|| DaemonError::UnknownVendor(vendor.to_string()))?;

        // Parse JSON config to serde_json::Value, then convert to toml::Value
        let json_value: serde_json::Value = serde_json::from_str(config_json)
            .map_err(|e| DaemonError::OperationFailed(format!("Invalid config JSON: {}", e)))?;

        // Schema-driven validation. Each pairing field declared by
        // the backend is canonically validated server-side before
        // pairing kicks off — see `PairingField::validate`. Failure
        // returns a typed `ConfigError::InvalidVendorConfig`
        // mapping to a `org.tisvlay.Error.ConfigInvalidVendorConfig`
        // D-Bus error so the GUI can surface a per-field message.
        let json_obj = json_value.as_object().ok_or_else(|| {
            DaemonError::OperationFailed("config_json must be a JSON object".to_string())
        })?;
        for field in backend.connector.pairing_schema() {
            // Coerce JSON → string for the validator. Numbers + bools
            // get their natural string repr so a port submitted as
            // `3001` (number) validates the same as `"3001"` (string).
            let value = match json_obj.get(&field.name) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                Some(serde_json::Value::Bool(b)) => b.to_string(),
                Some(serde_json::Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            field.validate(&value).map_err(DaemonError::from)?;
        }

        let vendor_config = json_to_toml(&json_value).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to convert config: {}", e))
        })?;

        let manager = &self.manager;
        let tv_id = manager
            .pair_tv(name, vendor, vendor_config, None, managed)
            .await
            .map_err(DaemonError::from)?;

        // Fetch live state from the connection established during pairing
        // so D-Bus interfaces are registered with real values, not zeroed
        // defaults. Without this, the InterfacesAdded signal and the first
        // GetManagedObjects call would show volume=0, backlight=0, etc.
        let state = manager.get_current_state(&tv_id).await;
        let path = tv_path(&tv_id);
        if let Err(e) = register_tv_interfaces(
            connection,
            &path,
            tv_id.clone(),
            manager.clone(),
            state.as_ref(),
        )
        .await
        {
            warn!(tv_id = %tv_id, error = %e, "Failed to register D-Bus interfaces for new TV");
        }

        Ok((ObjectPath::try_from(path).unwrap(), tv_id))
    }

    /// Remove a TV.
    async fn remove_tv(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        tv_path: ObjectPath<'_>,
    ) -> Result<(), DaemonError> {
        // Extract TV ID from path
        let path_str = tv_path.as_str();
        let tv_id = path_str
            .strip_prefix("/org/tisvlay/Tv/")
            .ok_or_else(|| DaemonError::TvNotFound(format!("Invalid path: {}", path_str)))?;

        info!(tv_id = %tv_id, "D-Bus: RemoveTv");

        let manager = &self.manager;
        manager.remove_tv(tv_id).await.map_err(DaemonError::from)?;

        // Unregister D-Bus interfaces for the removed TV.
        if let Err(e) = unregister_tv_interfaces(connection, path_str).await {
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

    /// Set a system-level TV option.
    ///
    /// Supported keys: `primary_input`, `managed`, `name`, `profile.system`.
    async fn set_tv_option(&self, tv_id: &str, key: &str, value: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, key = %key, value = %value, "D-Bus: SetTvOption");
        self.manager
            .set_tv_option(tv_id, key, value)
            .await
            .map_err(DaemonError::from)
    }

    /// Get the admin baseline for user profiles as JSON.
    ///
    /// Returns the `[profiles.default]` from the system config.
    /// Used by session services to inherit defaults when users
    /// have no config file.
    async fn get_user_profile_default(&self) -> Result<String, DaemonError> {
        debug!("D-Bus: GetUserProfileDefault");
        let profile = self.manager.get_user_profile_default().await;
        serde_json::to_string(&profile).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to serialize profile: {}", e))
        })
    }

    /// Get all system profiles as (name, json) pairs.
    ///
    /// Returns all system profiles defined in the system config.
    /// Used by tray, GUI, and CLI to display available system profiles.
    async fn list_system_profiles(&self) -> Result<Vec<(String, String)>, DaemonError> {
        debug!("D-Bus: ListSystemProfiles");
        let profiles = self.manager.list_system_profiles().await;
        profiles
            .into_iter()
            .map(|(name, profile)| {
                let json = serde_json::to_string(&profile).map_err(|e| {
                    DaemonError::OperationFailed(format!("Failed to serialize profile: {}", e))
                })?;
                Ok((name, json))
            })
            .collect()
    }

    /// Get all user profiles as (name, json) pairs.
    ///
    /// Returns all profiles defined in the system config.
    /// Used by session services to merge with user-defined profiles.
    async fn list_user_profiles(&self) -> Result<Vec<(String, String)>, DaemonError> {
        debug!("D-Bus: ListUserProfiles");
        let profiles = self.manager.list_user_profiles().await;
        profiles
            .into_iter()
            .map(|(name, profile)| {
                let json = serde_json::to_string(&profile).map_err(|e| {
                    DaemonError::OperationFailed(format!("Failed to serialize profile: {}", e))
                })?;
                Ok((name, json))
            })
            .collect()
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

    /// Signal emitted when configuration is reloaded.
    ///
    /// Subscribers should refresh profile lists and TV properties.
    #[zbus(signal)]
    pub async fn config_reloaded(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Pairing-flow phase transition.
    ///
    /// Emitted as `pair_tv()` walks through the underlying
    /// [`tisvlay_core::PairingState`]. `phase` is the stable D-Bus
    /// string from `PairingState::to_dbus_state()`: `awaiting_tv`,
    /// `awaiting_input`, `complete`, `failed`. `message` is a
    /// human-readable detail (e.g., "Please accept the pairing
    /// request on your TV", or the failure reason).
    ///
    /// Pair this with the synchronous `AddTv` RPC: subscribe to the
    /// signal first, then call `AddTv`, draining phases until the
    /// call returns.
    #[zbus(signal)]
    pub async fn pairing_progress(
        emitter: &SignalEmitter<'_>,
        tv_id: &str,
        phase: &str,
        message: &str,
    ) -> zbus::Result<()>;

    /// One TV was discovered during a `DiscoverTvs` scan.
    ///
    /// Wire payload mirrors `DiscoverTvs`'s return tuple:
    /// `(id, name, address, (manufacturer, model, serial), vendor,
    /// vendor_data)`. Empty strings represent unknown fields.
    /// `vendor_data` is the same `a{ss}` map carried on the RPC return
    /// (ARP-discovered MAC under `"mac"`, etc.). Listening to this
    /// signal lets clients render results progressively instead of
    /// waiting on the synchronous RPC return.
    ///
    /// In v1 the signals are emitted in a burst right before
    /// `DiscoverTvs` returns (no per-backend streaming yet); the
    /// wire shape is forward-compatible with future per-backend
    /// streaming refactors.
    #[zbus(signal)]
    pub async fn discovery_found(
        emitter: &SignalEmitter<'_>,
        id: &str,
        name: &str,
        address: &str,
        hardware_id: (&str, &str, &str),
        vendor: &str,
        vendor_data: HashMap<String, String>,
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
