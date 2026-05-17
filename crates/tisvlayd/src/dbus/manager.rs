//! D-Bus Manager interface implementation.
//!
//! Implements `org.tisvlay.Manager1` for TV discovery and management.

use crate::dbus::system_profile::{register_system_profile, unregister_system_profile};
use crate::dbus::{DaemonError, apply_reload_diff};
use crate::json_toml::json_to_toml;
use crate::registry::BackendRegistry;
use crate::tv_manager::TvManager;
use std::collections::HashMap;
use std::sync::Arc;
use tisvlay_core::dbus_paths::{system_profile_path, tv_path};

use tracing::{debug, info};
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
    // Identity
    // -------------------------------------------------------------------------

    /// Compile-time version of the running daemon binary. Lets the GUI
    /// (and other clients) surface installed-version drift between
    /// processes after a partial upgrade where services weren't
    /// restarted.
    #[zbus(property(emits_changed_signal = "const"))]
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Resolved on-disk path of the daemon's config file (post
    /// `--config` resolution). Empty string only if the path can't
    /// be UTF-8 encoded, which is theoretical on Linux.
    #[zbus(property(emits_changed_signal = "const"))]
    fn config_path(&self) -> String {
        self.manager
            .config_path()
            .to_str()
            .unwrap_or_default()
            .to_string()
    }

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

        // Spawn the pairing handshake. `pair_tv` returns the
        // reserved tv_id immediately (sync validation only); the
        // handshake itself runs in the background. Outcome surfaces
        // via `Manager1.PairingProgress` signals (filtered on
        // tv_id by clients) and via `ObjectManager.InterfacesAdded`
        // when the per-TV `Tv1.*` interfaces register on success.
        // The cloned `connection` is moved into the spawned task so
        // it can register interfaces without going back through
        // this handler.
        let tv_id = self
            .manager
            .pair_tv(name, vendor, vendor_config, managed, connection.clone())
            .await
            .map_err(DaemonError::from)?;

        let path = tv_path(&tv_id);
        Ok((ObjectPath::try_from(path).unwrap(), tv_id))
    }

    /// Reload configuration from disk.
    ///
    /// In addition to refreshing the in-memory state, syncs the D-Bus
    /// object server: any TV that vanished from the config gets its
    /// `Tv1.*` interfaces unregistered, any TV that appeared via a
    /// direct file edit gets fresh interfaces registered. Mirrors
    /// what `Manager1.AddTv` and `Tv1.Remove` already do for their
    /// respective single-TV paths.
    async fn reload_config(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Result<(), DaemonError> {
        info!("D-Bus: ReloadConfig");
        let manager = &self.manager;
        let diff = manager.reload_config().await.map_err(DaemonError::from)?;
        apply_reload_diff(connection, manager, &diff).await;
        Ok(())
    }

    /// Cancel an in-flight pairing handshake for the given TV.
    ///
    /// Reaches into the daemon's `pairing_sessions` map (populated
    /// inside the spawned `run_pairing_handshake` of `pair_tv` /
    /// `re_pair_tv`, removed on completion via the RAII
    /// `PairingSessionGuard`) and sends `PairingCommand::Cancel`
    /// via the session's command channel. The handshake task
    /// observes the cancel command, resolves the session with
    /// `PairingError::cancelled`, and the outer `run_pair_task` /
    /// `run_re_pair_task` emits a terminal `failed`
    /// `Manager1.PairingProgress` carrying the cancellation reason.
    /// The originating `Manager1.AddTv` / `Tv1.RePair` RPC has
    /// already returned `Ok` long before this — both methods are
    /// async-spawn, so cancellation outcomes surface via the
    /// `PairingProgress` channel, never as an RPC `Err`.
    ///
    /// Returns `Err(DaemonError::NotPairing(...))` if no in-flight
    /// session exists for the given tv_id (already completed, never
    /// started, wrong id). The error name is the stable routing key
    /// (`org.tisvlay.Error.NotPairing`); the detail body carries the
    /// `TvManagerError::NotPairing` Display message so CLI / logs
    /// are self-explanatory. Matches BlueZ's `Adapter.RemoveDevice`-
    /// on-unknown-device convention. Clients only fire cancel when
    /// they know a pair is in flight (they're listening to
    /// `PairingProgress` signals); the typed error helps debugging
    /// if that invariant is ever broken.
    ///
    /// Works for both `AddTv` and `Tv1.RePair`: each spawned task
    /// inserts its session's command sender into `pairing_sessions`
    /// when it reaches `run_pairing_handshake`, so cancel reaches
    /// the task via that map regardless of which RPC initiated the
    /// pair. The brief gap between the RPC returning and the insert
    /// landing is documented at
    /// [`crate::tv_manager::TvManager::cancel_pair`] ("Cancel during
    /// the spawn gap"); a cancel arriving in that window returns
    /// `NotPairing`.
    async fn cancel_pair(&self, tv_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, "D-Bus: CancelPair");
        self.manager
            .cancel_pair(tv_id)
            .await
            .map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // System Config Management
    // -------------------------------------------------------------------------

    /// Get the admin baseline for user profiles as JSON.
    ///
    /// Returns the `[profiles.default]` from the system config.
    /// Used by session services to inherit defaults when users
    /// have no config file. The daemon's user-side profile bodies
    /// aren't published as `UserProfile1` D-Bus objects (the session
    /// owns that surface on the user bus), so this method remains the
    /// admin-baseline transport for the session.
    async fn get_user_profile_default(&self) -> Result<String, DaemonError> {
        debug!("D-Bus: GetUserProfileDefault");
        let profile = self.manager.get_user_profile_default().await;
        serde_json::to_string(&profile).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to serialize profile: {}", e))
        })
    }

    /// Get all admin user profiles as (name, json) pairs.
    ///
    /// Returns all user-side profiles defined in the daemon's system
    /// config. Same admin-baseline role as `GetUserProfileDefault` —
    /// the session merges these with its own user profiles to drive
    /// the inheritance walk. Distinct from the v2 ObjectManager surface
    /// (the daemon has no `UserProfile1` D-Bus objects to enumerate;
    /// `UserProfile1` lives on the user bus owned by the session).
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
    // System-profile lifecycle
    // -------------------------------------------------------------------------

    /// Create a new system profile with the given friendly name and
    /// every field inheriting. Returns the new object path; the
    /// caller can build a `PropertiesProxy` against the path or read
    /// the newly-added entry from `ObjectManager.GetManagedObjects`.
    ///
    /// Rejects empty names, names equal to
    /// [`DEFAULT_PROFILE_NAME`](tisvlay_core::DEFAULT_PROFILE_NAME),
    /// and duplicates — all as the standard
    /// `org.freedesktop.DBus.Error.InvalidArgs` so GUI clients route
    /// on the well-known error name without parsing the message.
    async fn create_system_profile(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        name: &str,
    ) -> zbus::fdo::Result<ObjectPath<'static>> {
        info!(name = %name, "D-Bus: CreateSystemProfile");
        let uuid = self
            .manager
            .create_system_profile(name.to_string())
            .await
            .map_err(create_system_profile_err)?;

        // Register the per-profile interface so `InterfacesAdded`
        // fires for clients watching the catalog.
        if let Err(e) = register_system_profile(connection, uuid, self.manager.clone()).await {
            tracing::warn!(uuid = %uuid, error = %e, "Failed to register SystemProfile interface");
        }

        let path = system_profile_path(&uuid);
        ObjectPath::try_from(path)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to build profile path: {}", e)))
    }

    /// Delete an existing system profile by UUID.
    ///
    /// Malformed UUIDs return the standard
    /// `org.freedesktop.DBus.Error.InvalidArgs`. Deleting the default
    /// profile or one currently referenced by a TV returns
    /// `org.freedesktop.DBus.Error.AccessDenied`; the message body
    /// names the offending TV UUIDs so the GUI's delete-confirm
    /// dialog can surface them.
    async fn delete_system_profile(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        uuid: &str,
    ) -> zbus::fdo::Result<()> {
        info!(uuid = %uuid, "D-Bus: DeleteSystemProfile");
        let parsed: uuid::Uuid = uuid.parse().map_err(|_| {
            zbus::fdo::Error::InvalidArgs(format!("Invalid system profile UUID: {uuid}"))
        })?;

        self.manager
            .delete_system_profile(parsed)
            .await
            .map_err(delete_system_profile_err)?;

        // Drop the per-profile interface so `InterfacesRemoved` fires.
        unregister_system_profile(connection, parsed).await;
        Ok(())
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
    /// Pair this with the async-spawn `AddTv` / `Tv1.RePair` RPCs:
    /// subscribe to the signal first, call the RPC to get the
    /// reserved `tv_id`, then drain the signal stream filtered on
    /// that tv_id to a terminal `complete` / `failed` phase (see
    /// `DaemonClient::drain_pair_to_terminal`).
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

/// Map `TvManager::create_system_profile` errors to standard fdo
/// names. Validation failures (empty / reserved / duplicate name)
/// land on `InvalidArgs`; anything else goes to `Failed`.
fn create_system_profile_err(e: crate::tv_manager::TvManagerError) -> zbus::fdo::Error {
    use crate::tv_manager::TvManagerError;
    match e {
        TvManagerError::ConfigError(msg) => zbus::fdo::Error::InvalidArgs(msg),
        other => zbus::fdo::Error::Failed(other.to_string()),
    }
}

/// Map `TvManager::delete_system_profile` errors to standard fdo
/// names. Reserved + in-use rejections become `AccessDenied`; an
/// unknown UUID becomes `InvalidArgs`; anything else `Failed`.
fn delete_system_profile_err(e: crate::tv_manager::TvManagerError) -> zbus::fdo::Error {
    use crate::tv_manager::TvManagerError;
    match e {
        TvManagerError::ProfileReserved(msg) => zbus::fdo::Error::AccessDenied(msg),
        TvManagerError::ProfileInUse { name, tvs } => zbus::fdo::Error::AccessDenied(format!(
            "profile '{}' is referenced by TVs: {}",
            name,
            tvs.join(", ")
        )),
        TvManagerError::ConfigError(msg) => zbus::fdo::Error::InvalidArgs(msg),
        other => zbus::fdo::Error::Failed(other.to_string()),
    }
}
