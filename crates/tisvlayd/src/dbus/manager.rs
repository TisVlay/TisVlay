//! D-Bus Manager interface implementation.
//!
//! Implements `org.tisvlay.Manager1` for TV discovery and management.

use crate::dbus::{DaemonError, apply_reload_diff};
use crate::json_toml::json_to_toml;
use crate::registry::BackendRegistry;
use crate::tv_manager::TvManager;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tisvlay_core::dbus_paths::{MANAGER_PATH, tv_path};
use tisvlay_core::{DiscoveredTv, SystemProfileBody};
use tokio::sync::Notify;
use uuid::Uuid;

use tracing::{debug, info, warn};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;

/// Hard upper bound on any single streaming discovery session, regardless
/// of the client's requested timeout. Backstop against a GUI that crashed
/// after `StartDiscovery` without ever calling `StopDiscovery`.
const STREAMING_DISCOVERY_HARD_CAP: Duration = Duration::from_secs(5 * 60);

/// Per-iteration M-SEARCH timeout inside the streaming loop. Each
/// iteration sends one burst then waits this long for responses.
const STREAMING_SCAN_TIMEOUT: Duration = Duration::from_secs(5);

/// Budget for the post-probe poll loop in `EnrichManualEntry`. The
/// kernel typically resolves a fresh ARP entry in tens of ms on a
/// healthy LAN; only the "TV unreachable" path rides the full
/// budget. The polling is the cap, not the typical wall-time.
const ARP_RESOLVE_BUDGET: Duration = Duration::from_millis(500);

/// Outer wall-time cap on the whole `EnrichManualEntry` chain.
/// Daemon-side backstop independent of the wizard's own timeout.
const ENRICH_MANUAL_ENTRY_CAP: Duration = Duration::from_millis(600);

/// D-Bus Manager service.
///
/// Provides TV management operations at `/org/tisvlay/Manager`.
#[derive(Debug)]
pub struct ManagerService {
    manager: Arc<TvManager>,
    registry: Arc<BackendRegistry>,
    /// Streaming discovery sessions started by
    /// `Manager1.StartDiscovery`. Keyed by session id (UUID); the
    /// value is a cancel-notify used by `StopDiscovery` and by the
    /// session task's hard-cap path. Cleared by the session task on
    /// any exit (cancel, timeout, hard cap).
    discovery_sessions: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    /// Per-iteration period for streaming discovery, read from
    /// `[discovery] poll_interval_ms` at daemon startup.
    poll_interval: Duration,
}

impl ManagerService {
    /// Create a new manager service.
    pub fn new(
        manager: Arc<TvManager>,
        registry: Arc<BackendRegistry>,
        poll_interval: Duration,
    ) -> Self {
        Self {
            manager,
            registry,
            discovery_sessions: Arc::new(Mutex::new(HashMap::new())),
            poll_interval,
        }
    }
}

/// Enrich a discovered TV with vendor_data["mac"] when an IP-addressed
/// entry didn't already get one from its vendor backend. SSDP
/// responses prime the ARP cache, so a passive kernel-neighbor lookup
/// usually suffices.
async fn enrich_with_arp(mut tv: DiscoveredTv) -> DiscoveredTv {
    if !tv.vendor_data.contains_key("mac")
        && let Ok(ip) = tv.address.parse::<std::net::IpAddr>()
        && let Some(mac) = crate::netlink::arp_lookup(ip).await
    {
        tv.vendor_data.insert("mac".to_string(), mac.to_string());
    }
    tv
}

/// Drive the manual-entry enrichment chain: validate input, gate to
/// IPv4, passive ARP lookup, vendor-controlled active probe on miss,
/// poll netlink with deadline. Always returns — empty map for any
/// "nothing to do" outcome (invalid IP, IPv6 input, unknown vendor,
/// or the ARP chain found nothing).
///
/// Free function for testability and so the RPC method body stays
/// thin. Wrapped in an outer `tokio::time::timeout` so even a
/// pathological netlink stall can't burst past `ENRICH_MANUAL_ENTRY_CAP`.
async fn enrich_manual_entry(
    registry: &BackendRegistry,
    vendor: &str,
    address: &str,
) -> HashMap<String, String> {
    let chain = enrich_manual_entry_inner(registry, vendor, address);
    tokio::time::timeout(ENRICH_MANUAL_ENTRY_CAP, chain)
        .await
        .unwrap_or_default()
}

async fn enrich_manual_entry_inner(
    registry: &BackendRegistry,
    vendor: &str,
    address: &str,
) -> HashMap<String, String> {
    // (a) Empty-input fast path. Defensive against misfired callbacks.
    if vendor.is_empty() || address.is_empty() {
        return HashMap::new();
    }

    // (b) Parse the address (any IP family). Malformed input
    //     collapses to empty.
    let Ok(ip) = address.parse::<std::net::IpAddr>() else {
        return HashMap::new();
    };

    // (c) Vendor lookup. Unknown vendors collapse to empty.
    let Some(registered) = registry.get(vendor) else {
        return HashMap::new();
    };

    // (d) Capability-aware family gate. Vendors declare which IP
    //     families they support via `Capabilities::IPV4`/`IPV6`
    //     (see docs/discovery.md § "Per-vendor transport family").
    //     Inputs whose family the vendor hasn't claimed collapse to
    //     empty — symmetric with the wizard's form-build-time gating,
    //     so a v6 address typed against a v4-only vendor never gets a
    //     no-op probe.
    let caps = registered.backend.capabilities();
    let family_supported = match ip {
        std::net::IpAddr::V4(_) => caps.contains(tisvlay_core::Capabilities::IPV4),
        std::net::IpAddr::V6(_) => caps.contains(tisvlay_core::Capabilities::IPV6),
    };
    if !family_supported {
        return HashMap::new();
    }
    let connector = registered.connector.as_ref();

    let span = tracing::info_span!("enrich_manual_entry", %vendor, %ip);
    let _enter = span.enter();
    let started = Instant::now();

    // (d) Passive netlink read (free; succeeds for users who've
    //     previously pinged/browsed the TV).
    if let Some(mac) = crate::netlink::arp_lookup(ip).await {
        info!(
            source = "passive",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "MAC resolved"
        );
        return mac_result(mac);
    }

    // (e) Vendor-supplied active probe. Connector decides mechanism
    //     (UDP byte to SSAP port for LG). Default impl is a no-op;
    //     probe-less vendors degrade gracefully to passive-only.
    //     Err is non-fatal — netlink re-read might still hit.
    if let Err(e) = connector.probe_arp_cache(ip).await {
        debug!(error = %e, "probe_arp_cache failed; continuing to poll");
    }

    // (f) Poll netlink with deadline. Beats a fixed sleep: cache
    //     typically resolves in tens of ms on a healthy LAN; only
    //     the "TV unreachable" case rides the full budget.
    let deadline = Instant::now() + ARP_RESOLVE_BUDGET;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if let Some(mac) = crate::netlink::arp_lookup(ip).await {
            info!(
                source = "active",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "MAC resolved"
            );
            return mac_result(mac);
        }
    }
    info!(
        source = "none",
        elapsed_ms = started.elapsed().as_millis() as u64,
        "MAC not resolved"
    );
    HashMap::new()
}

fn mac_result(mac: tisvlay_core::MacAddress) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert("mac".to_string(), mac.to_string());
    out
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
        // the kernel neighbor table. Shared with the streaming
        // `StartDiscovery` loop via the `enrich_with_arp` helper.
        let mut enriched = Vec::with_capacity(tvs.len());
        for tv in tvs {
            enriched.push(enrich_with_arp(tv).await);
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

    /// Start a streaming discovery session.
    ///
    /// Spawns a daemon-side task that loops M-SEARCH bursts at the
    /// configured `[discovery] poll_interval_ms` (default 8 s) and
    /// emits `DiscoveryFound` for each newly-seen TV. The session
    /// runs until `StopDiscovery(session_id)`, until `timeout_ms`
    /// elapses, or until the 5-minute server-side hard cap fires
    /// (whichever first). `timeout_ms = 0` means "until stopped or
    /// hard cap" — the natural choice for a pairing wizard that
    /// stays open while the user walks across the room to power on
    /// the TV.
    ///
    /// Within one session the same `(vendor, id)` pair is emitted
    /// only once — daemon-side dedup. Across overlapping sessions
    /// the same TV may be emitted by each session; client-side
    /// dedup is the consumer's responsibility.
    ///
    /// Returns the `session_id` the caller must pass to
    /// `StopDiscovery`.
    #[cfg(feature = "discovery")]
    async fn start_discovery(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        timeout_ms: u32,
    ) -> Result<String, DaemonError> {
        let session_id = Uuid::new_v4().to_string();
        let cancel = Arc::new(Notify::new());

        self.discovery_sessions
            .lock()
            .insert(session_id.clone(), cancel.clone());

        info!(
            session_id = %session_id,
            timeout_ms,
            poll_interval_ms = self.poll_interval.as_millis(),
            "D-Bus: StartDiscovery",
        );

        let registry = self.registry.clone();
        let sessions = self.discovery_sessions.clone();
        let conn = connection.clone();
        let sid = session_id.clone();
        let poll_interval = self.poll_interval;
        let soft_deadline = if timeout_ms > 0 {
            Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
        } else {
            None
        };

        tokio::spawn(async move {
            run_discovery_session(
                sid,
                conn,
                registry,
                sessions,
                cancel,
                poll_interval,
                soft_deadline,
            )
            .await;
        });

        Ok(session_id)
    }

    /// Stop a streaming discovery session.
    ///
    /// Idempotent — an unknown `session_id` is not an error (the
    /// session may have already hit its deadline or hard cap).
    #[cfg(feature = "discovery")]
    async fn stop_discovery(&self, session_id: String) -> Result<(), DaemonError> {
        debug!(session_id = %session_id, "D-Bus: StopDiscovery");
        if let Some(cancel) = self.discovery_sessions.lock().remove(&session_id) {
            cancel.notify_one();
        }
        Ok(())
    }

    /// Enrich a user-typed (vendor, IP) pair with auto-detected
    /// values for the pairing form — currently just `vendor_data["mac"]`
    /// via a kernel ARP-cache read with vendor-controlled active
    /// priming as a fallback.
    ///
    /// Returns the same `vendor_data`-shaped map that `DiscoveryFound`
    /// carries, so callers can feed it through their existing
    /// schema-form autofill engine uniformly with the SSDP path.
    ///
    /// Always returns `Ok`. Empty map means "nothing to autofill" —
    /// invalid IP, vendor-unsupported family (per the vendor's
    /// `Capabilities::IPV4`/`IPV6` declaration), out-of-subnet,
    /// unknown vendor, or the ARP/NDP chain found nothing. The
    /// wizard treats empty as a soft outcome (field stays blank
    /// with a "MAC not detected" helper) rather than a hard
    /// failure.
    async fn enrich_manual_entry(
        &self,
        vendor: String,
        address: String,
    ) -> Result<HashMap<String, String>, DaemonError> {
        Ok(enrich_manual_entry(&self.registry, &vendor, &address).await)
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

    /// Get the `Capabilities` bitflag for one vendor as the same
    /// kebab-case string list `Capabilities::to_strings()` emits
    /// ("power", "screen", "ipv4", "ipv6", …). Unknown vendor →
    /// empty list (not an error — the GUI's wizard treats absent
    /// caps as "feature unavailable", which is the safe default).
    ///
    /// Additive sibling to `GetSupportedVendors` rather than a
    /// breaking shape change to its tuple — keeps existing clients
    /// working without a wire negotiation.
    fn get_vendor_capabilities(&self, vendor: String) -> Vec<String> {
        debug!(vendor = %vendor, "D-Bus: GetVendorCapabilities");
        self.registry
            .get(&vendor)
            .map(|registered| {
                registered
                    .backend
                    .capabilities()
                    .to_strings()
                    .into_iter()
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
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
    /// have no config file.
    async fn get_user_profile_default(&self) -> Result<String, DaemonError> {
        debug!("D-Bus: GetUserProfileDefault");
        let profile = self.manager.get_user_profile_default().await;
        serde_json::to_string(&profile).map_err(|e| {
            DaemonError::OperationFailed(format!("Failed to serialize profile: {}", e))
        })
    }

    /// Get all system profiles as `(name, json)` pairs. The JSON
    /// serializes a `SystemProfileConfig`. Bound to the
    /// `ListSystemProfilesJson` wire slot; the `ListSystemProfiles`
    /// wire name carries the property-bag `a{sa{sv}}` shape.
    #[zbus(name = "ListSystemProfilesJson")]
    async fn list_system_profiles(&self) -> Result<Vec<(String, String)>, DaemonError> {
        debug!("D-Bus: ListSystemProfilesJson");
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
    // System profile property-bag CRUD
    //
    // Bodies round-trip as `SystemProfileBody` (`a{sv}` via dict-derive);
    // the catalog list is `a{sa{sv}}` (uuid string → body dict). Eight
    // methods + three signals anchored on `Manager1`.
    // -------------------------------------------------------------------------

    /// Catalog list: every system profile keyed by UUID string. Wire
    /// shape `a{sa{sv}}`.
    #[zbus(name = "ListSystemProfiles")]
    async fn list_system_profile_bodies(
        &self,
    ) -> Result<HashMap<String, SystemProfileBody>, DaemonError> {
        debug!("D-Bus: ListSystemProfiles");
        let bodies = self.manager.list_system_profile_bodies().await;
        Ok(bodies
            .into_iter()
            .map(|(uuid, body)| (uuid.to_string(), body))
            .collect())
    }

    /// Fetch one body by UUID string. Returns `ProfileNotFound` if
    /// the UUID is malformed or absent from the catalog.
    async fn get_system_profile(&self, uuid: &str) -> Result<SystemProfileBody, DaemonError> {
        debug!(uuid = %uuid, "D-Bus: GetSystemProfile");
        let parsed = parse_profile_uuid(uuid)?;
        self.manager
            .get_system_profile_body(parsed)
            .await
            .map_err(DaemonError::from)
    }

    /// Atomic full-body replace. Rename folds into this method — the
    /// new name lives on `body.name`.
    async fn update_system_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uuid: &str,
        body: SystemProfileBody,
    ) -> Result<(), DaemonError> {
        info!(uuid = %uuid, name = %body.name, "D-Bus: UpdateSystemProfile");
        let parsed = parse_profile_uuid(uuid)?;
        self.manager
            .update_system_profile(parsed, body)
            .await
            .map_err(DaemonError::from)?;
        if let Err(e) = Self::system_profile_changed(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "Failed to emit SystemProfileChanged");
        }
        Ok(())
    }

    /// Mint a new UUID, insert an empty body under the given display
    /// name, persist, emit `SystemProfileAdded(uuid)`.
    async fn create_system_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        name: &str,
    ) -> Result<String, DaemonError> {
        info!(name = %name, "D-Bus: CreateSystemProfile");
        let new_uuid = self
            .manager
            .create_system_profile(name.to_string())
            .await
            .map_err(DaemonError::from)?;
        let uuid_str = new_uuid.to_string();
        if let Err(e) = Self::system_profile_added(&emitter, &uuid_str).await {
            warn!(uuid = %uuid_str, error = %e, "Failed to emit SystemProfileAdded");
        }
        Ok(uuid_str)
    }

    /// Clone an existing body under a new UUID + name, persist, emit
    /// `SystemProfileAdded(new_uuid)`. The new body has every field
    /// from the source except `name`.
    async fn duplicate_system_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        source_uuid: &str,
        new_name: &str,
    ) -> Result<String, DaemonError> {
        info!(source_uuid = %source_uuid, new_name = %new_name, "D-Bus: DuplicateSystemProfile");
        let parsed = parse_profile_uuid(source_uuid)?;
        let new_uuid = self
            .manager
            .duplicate_system_profile(parsed, new_name.to_string())
            .await
            .map_err(DaemonError::from)?;
        let uuid_str = new_uuid.to_string();
        if let Err(e) = Self::system_profile_added(&emitter, &uuid_str).await {
            warn!(uuid = %uuid_str, error = %e, "Failed to emit SystemProfileAdded");
        }
        Ok(uuid_str)
    }

    /// Delete a profile. Reject the reserved default UUID, reject if
    /// at least one TV references the target (explicit or implicit-
    /// default fan-out), persist, emit `SystemProfileRemoved(uuid)`.
    async fn delete_system_profile(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        uuid: &str,
    ) -> Result<(), DaemonError> {
        info!(uuid = %uuid, "D-Bus: DeleteSystemProfile");
        let parsed = parse_profile_uuid(uuid)?;
        self.manager
            .delete_system_profile(parsed)
            .await
            .map_err(DaemonError::from)?;
        if let Err(e) = Self::system_profile_removed(&emitter, uuid).await {
            warn!(uuid = %uuid, error = %e, "Failed to emit SystemProfileRemoved");
        }
        Ok(())
    }

    /// Hard-coded baseline: the body every profile inherits from when
    /// its fields are all `None`. Static, but exposed so clients don't
    /// have to duplicate the table — `gettext("default")` is the
    /// client-side concern.
    fn get_resolved_system_profile_defaults(&self) -> SystemProfileBody {
        debug!("D-Bus: GetResolvedSystemProfileDefaults");
        TvManager::resolved_system_profile_defaults()
    }

    /// Per-TV resolved walk: returns the fully-resolved body for
    /// the named TV's assigned profile (per-TV body → default body
    /// → hardcoded baseline). Used by the CLI's `tisvlay profile
    /// show <tv>` and any client that wants the resolved-by-server
    /// view without computing the chain itself.
    async fn get_effective_system_profile(
        &self,
        tv_id: &str,
    ) -> Result<SystemProfileBody, DaemonError> {
        debug!(tv_id = %tv_id, "D-Bus: GetEffectiveSystemProfile");
        self.manager
            .get_effective_system_profile(tv_id)
            .await
            .map_err(DaemonError::from)
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

    /// Fired after a system-profile lifecycle hook executes
    /// successfully on a TV (e.g. logind suspend → `on_sleep` →
    /// screen-off on the living-room TV; daemon boot → `on_startup`).
    /// Drives the tray's `automation_fired` notification.
    ///
    /// `event_kind` and `action_kind` are **stable snake_case
    /// identifiers** intended as gettext lookup keys
    /// (`automation.event.on_startup`,
    /// `automation.action.screen_off`) — NOT translated strings.
    /// They come from [`crate::tv_manager::SystemEvent::fmt`] and
    /// [`tisvlay_core::SessionAction::fmt`] respectively.
    ///
    /// `profile_kind` is the literal string `"system"` for this
    /// signal (the session service's `Session1.AutomationFired`
    /// sibling carries `"user"`). `profile_uuid` is the UUID of the
    /// system profile whose rule resolved to the fired action — lets
    /// the tray name the profile in the notification body without a
    /// re-resolve.
    ///
    /// Deliberate exception to the project's payload-less
    /// `StateEvent` convention: this is an ephemeral *event*, not a
    /// state change — there's nothing to re-query, so the payload
    /// travels in-band.
    #[zbus(signal)]
    pub async fn automation_fired(
        emitter: &SignalEmitter<'_>,
        tv_id: &str,
        event_kind: &str,
        action_kind: &str,
        profile_kind: &str,
        profile_uuid: &str,
    ) -> zbus::Result<()>;

    /// A system profile's body was mutated (any field, including
    /// `name` — rename folds into `UpdateSystemProfile`).
    ///
    /// Clients with a cached body for this UUID should re-fetch via
    /// `GetSystemProfile(uuid)`. Fires once per successful
    /// `UpdateSystemProfile` after persistence; clients are guaranteed
    /// to be able to fetch the new body the moment the signal arrives.
    #[zbus(signal)]
    pub async fn system_profile_changed(
        emitter: &SignalEmitter<'_>,
        uuid: &str,
    ) -> zbus::Result<()>;

    /// A new system profile appeared in the catalog (Create or
    /// Duplicate). Body is fetchable by the moment the signal
    /// arrives — both flows commit body + emit under the same
    /// write lock.
    #[zbus(signal)]
    pub async fn system_profile_added(emitter: &SignalEmitter<'_>, uuid: &str) -> zbus::Result<()>;

    /// A system profile was removed from the catalog. Clients should
    /// drop any cached body for this UUID.
    #[zbus(signal)]
    pub async fn system_profile_removed(
        emitter: &SignalEmitter<'_>,
        uuid: &str,
    ) -> zbus::Result<()>;

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

/// Parse a UUID string coming off the system-profile CRUD wire
/// surface. Malformed UUIDs return `ProfileNotFound` rather than a
/// generic `InvalidArgs` so clients can route the "unknown profile"
/// case uniformly whether the UUID is well-formed-but-absent or
/// just garbage.
fn parse_profile_uuid(value: &str) -> Result<Uuid, DaemonError> {
    Uuid::parse_str(value).map_err(|_| DaemonError::ProfileNotFound(value.to_string()))
}

/// Drive one streaming `Manager1.StartDiscovery` session: M-SEARCH
/// burst → emit `DiscoveryFound` for each newly-seen TV → wait for
/// next burst or cancel, repeat. Exits on any of: caller-issued
/// `StopDiscovery` (notify), caller-supplied soft deadline, the
/// `STREAMING_DISCOVERY_HARD_CAP` server-side backstop, or a
/// stop-emitting-and-fail mode that hits more than a handful of
/// consecutive iteration errors. Always cleans its session-map
/// entry on the way out.
#[cfg(feature = "discovery")]
async fn run_discovery_session(
    session_id: String,
    connection: zbus::Connection,
    registry: Arc<BackendRegistry>,
    sessions: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    cancel: Arc<Notify>,
    poll_interval: Duration,
    soft_deadline: Option<Instant>,
) {
    let started = Instant::now();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    loop {
        if started.elapsed() >= STREAMING_DISCOVERY_HARD_CAP {
            info!(session_id = %session_id, "Discovery session hit hard cap");
            break;
        }
        if let Some(deadline) = soft_deadline
            && Instant::now() >= deadline
        {
            debug!(session_id = %session_id, "Discovery session reached soft deadline");
            break;
        }

        // One iteration: discover_all blocks for STREAMING_SCAN_TIMEOUT,
        // but cancel during the scan unblocks immediately — we abandon
        // any in-flight per-response work for this iteration. The
        // daemon stops emitting once we exit, so abandoned partial
        // responses for *this* session are not surfaced anyway.
        let scan = registry.discover_all(STREAMING_SCAN_TIMEOUT);
        let tvs = tokio::select! {
            biased;
            _ = cancel.notified() => {
                debug!(session_id = %session_id, "Discovery session cancelled mid-scan");
                break;
            }
            res = scan => match res {
                Ok(v) => v,
                Err(e) => {
                    warn!(session_id = %session_id, error = %e, "Discovery iteration failed");
                    Vec::new()
                }
            },
        };

        for tv in tvs {
            let key = (tv.vendor.clone(), tv.id.clone());
            if !seen.insert(key) {
                continue;
            }
            let tv = enrich_with_arp(tv).await;
            let hardware_id = tv.hardware_id.clone().unwrap_or_default().to_dbus_tuple();
            let Ok(emitter) = SignalEmitter::new(&connection, MANAGER_PATH) else {
                warn!("Could not build SignalEmitter for streaming DiscoveryFound");
                continue;
            };
            if let Err(e) = ManagerService::discovery_found(
                &emitter,
                &tv.id,
                &tv.name,
                &tv.address,
                (&hardware_id.0, &hardware_id.1, &hardware_id.2),
                &tv.vendor,
                tv.vendor_data,
            )
            .await
            {
                warn!(error = %e, "Streaming DiscoveryFound emit failed");
            }
        }

        // No additional sleep between scans — the scan's own timeout
        // is the iteration period. `poll_interval` is therefore the
        // *minimum* time between successive M-SEARCH bursts; the
        // current default (8 s) gives ~8 s between bursts (the scan
        // takes ≤ STREAMING_SCAN_TIMEOUT = 5 s, then the next iteration
        // sleeps the remainder if poll_interval > scan timeout).
        let remaining = poll_interval.saturating_sub(STREAMING_SCAN_TIMEOUT);
        if !remaining.is_zero() {
            tokio::select! {
                biased;
                _ = cancel.notified() => {
                    debug!(session_id = %session_id, "Discovery session cancelled between scans");
                    break;
                }
                _ = tokio::time::sleep(remaining) => {}
            }
        }
    }

    // Remove our session-map entry whether or not StopDiscovery already
    // did so — keeps the map from leaking on deadline/hard-cap exits.
    sessions.lock().remove(&session_id);
    info!(session_id = %session_id, "Discovery session ended");
}
