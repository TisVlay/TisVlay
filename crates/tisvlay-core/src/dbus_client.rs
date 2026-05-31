//! D-Bus client for communicating with tisvlayd system daemon.
//!
//! This module provides a client for the tisvlay D-Bus interfaces:
//! - `org.tisvlay.Manager1` - TV management (list, discover, add, remove)
//! - `org.tisvlay.Tv1.*` - Per-TV control interfaces
//!
//! # Example
//!
//! ```no_run
//! use tisvlay_core::DaemonClient;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let client = DaemonClient::new_system().await?;
//!
//!     // List all configured TVs
//!     let tvs = client.list_tvs().await?;
//!     for tv in &tvs {
//!         println!("{}: {} - {}", tv.uuid, tv.name,
//!             if tv.connected { "connected" } else { "disconnected" });
//!     }
//!
//!     // Turn off screen on first TV
//!     if let Some(tv) = tvs.first() {
//!         client.screen_off(&tv.uuid).await?;
//!     }
//!
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result};
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};
use uuid::Uuid;
use zbus::fdo::ObjectManagerProxy;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::dbus_paths::{DAEMON_SERVICE, MANAGER_PATH, OBJECT_MANAGER_PATH, TV_BASE_PATH, tv_path};
use crate::{NotificationSettingsBody, SystemProfileBody, UserProfileBody};

// =============================================================================
// Bus Selection
// =============================================================================

/// Which bus to build a D-Bus connection on.
///
/// `Session` and `System` use `zbus::connection::Builder::session()` /
/// `system()` respectively. `Address` takes an arbitrary `unix:path=...`
/// string (typically a private `dbus-daemon` launched by the test
/// harness) and bypasses `zbus`'s per-process `OnceCell` cache.
#[derive(Debug, Clone, Copy)]
pub enum BusKind<'a> {
    Session,
    System,
    Address(&'a str),
}

impl BusKind<'_> {
    /// Create a `zbus::connection::Builder` for this bus kind.
    pub fn builder(&self) -> zbus::Result<zbus::connection::Builder<'_>> {
        match self {
            BusKind::Session => zbus::connection::Builder::session(),
            BusKind::System => zbus::connection::Builder::system(),
            BusKind::Address(addr) => zbus::connection::Builder::address(*addr),
        }
    }
}

// =============================================================================
// Pairing terminal outcome
// =============================================================================

/// Terminal outcome of a `Manager1.PairingProgress` stream for a
/// single `tv_id`.
///
/// Returned by [`DaemonClient::drain_pair_to_terminal`] when the
/// stream reaches a `complete` or `failed` phase for the target
/// tv_id. The `tv_id` field echoes the value the caller filtered
/// against (so the same tuple is self-contained for logging). The
/// `message` field is the success confirmation for `Complete` and
/// the failure reason for `Failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairTerminal {
    /// Pairing completed successfully. `message` carries the
    /// backend's confirmation string (typically `"Pairing complete"`).
    Complete { tv_id: String, message: String },
    /// Pairing failed (TV rejected, timeout, cancelled, network
    /// error, …). `message` carries the human-readable failure
    /// reason from the originating `PairingError`.
    Failed { tv_id: String, message: String },
}

// =============================================================================
// Manager Interface Proxy
// =============================================================================

/// D-Bus proxy for the tisvlay Manager interface.
#[proxy(
    interface = "org.tisvlay.Manager1",
    default_service = "org.tisvlay.Daemon",
    default_path = "/org/tisvlay/Manager"
)]
trait TisvlayManager {
    /// Compile-time version of the running daemon binary.
    ///
    /// Marked `emits_changed_signal = "false"` so zbus doesn't cache
    /// the read: const-property caches survive the peer's
    /// `NameOwnerChanged`, which would let consumers (e.g. the GUI's
    /// About-pane refresh) report a gone peer as still online. Same
    /// rationale applies to every other peer `Version` / `ConfigPath`
    /// property below.
    #[zbus(property(emits_changed_signal = "false"))]
    fn version(&self) -> zbus::Result<String>;

    /// Resolved on-disk path of the daemon's config file (post `--config`
    /// resolution). Empty string when path resolution fails (theoretical
    /// on Linux).
    #[zbus(property(emits_changed_signal = "false"))]
    fn config_path(&self) -> zbus::Result<String>;

    /// Discover TVs on the network.
    ///
    /// Returns `(id, name, address, (manufacturer, model, serial),
    /// vendor, vendor_data)` tuples. The nested `(sss)` carries the
    /// vendor-reported hardware identity with empty strings for
    /// unknown fields. `vendor_data` is a free-form `key → value`
    /// map of vendor-specific discovery data — currently used to
    /// carry the kernel-resolved MAC under `"mac"` for IP-addressed
    /// backends. Wizards match these entries against each vendor's
    /// `PairingField::autofill` directives.
    fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> zbus::Result<
        Vec<(
            String,
            String,
            String,
            (String, String, String),
            String,
            std::collections::HashMap<String, String>,
        )>,
    >;

    /// Start a streaming discovery session.
    ///
    /// Daemon-side: spawns a task that loops M-SEARCH bursts at the
    /// configured `[discovery] poll_interval_ms` (default 8 s) and
    /// emits `DiscoveryFound` for each newly-seen TV. Each TV is
    /// emitted at most once per session (daemon-side dedup).
    ///
    /// `timeout_ms = 0` runs until `StopDiscovery` or the 5-minute
    /// server-side hard cap, whichever first — typical for a pairing
    /// wizard that stays open while the user walks to the TV.
    /// Non-zero is a soft deadline; the session ends sooner of stop /
    /// soft deadline / hard cap.
    ///
    /// Returns the session id the caller passes to `StopDiscovery`.
    fn start_discovery(&self, timeout_ms: u32) -> zbus::Result<String>;

    /// Stop a streaming discovery session.
    ///
    /// Idempotent — an unknown session id is not an error.
    fn stop_discovery(&self, session_id: &str) -> zbus::Result<()>;

    /// Enrich a user-typed (vendor, address) pair for the pairing
    /// wizard's manual-entry path. Returns the same `vendor_data`-
    /// shaped map that `DiscoveryFound` carries (currently just
    /// `"mac"` when ARP resolution succeeds, empty otherwise).
    /// Always succeeds at the wire layer; empty map means "nothing
    /// to autofill" — invalid IP, IPv6 input, out-of-subnet,
    /// unknown vendor, or the ARP chain found nothing.
    fn enrich_manual_entry(
        &self,
        vendor: &str,
        address: &str,
    ) -> zbus::Result<std::collections::HashMap<String, String>>;

    /// Get supported vendors.
    /// Returns: [(vendor_id, vendor_name)]
    fn get_supported_vendors(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Get the `Capabilities` bitflag for one vendor as the same
    /// kebab-case list `Capabilities::to_strings()` emits. Unknown
    /// vendor → empty list (treated as "feature unavailable" by
    /// the caller — never an error at the wire layer). Sibling to
    /// `get_supported_vendors` so existing clients keep working.
    fn get_vendor_capabilities(&self, vendor: &str) -> zbus::Result<Vec<String>>;

    /// Get the pairing-form schema for a vendor.
    ///
    /// Returns a JSON-serialized `Vec<PairingField>` describing the
    /// fields the wizard should render (and the daemon expects in
    /// the matching `add_tv` call's `config_json`). See
    /// [`crate::pairing_schema`] for the field shape.
    fn get_pairing_schema(&self, vendor: &str) -> zbus::Result<String>;

    /// Add a TV.
    /// config_json: vendor-specific connection parameters as JSON
    /// managed: whether the TV participates in automation
    /// Returns: (object_path, tv_id)
    fn add_tv(
        &self,
        name: &str,
        vendor: &str,
        config_json: &str,
        managed: bool,
    ) -> zbus::Result<(OwnedObjectPath, String)>;

    /// Reload configuration.
    fn reload_config(&self) -> zbus::Result<()>;

    /// Cancel an in-flight pairing handshake for the given TV.
    /// Returns `Err(NotPairing)` if no in-flight session exists.
    /// Works for both `AddTv` and `Tv1.RePair`: cancel reaches the
    /// spawned task's pairing session via the daemon's
    /// `pairing_sessions` map and resolves the handshake with a
    /// cancelled error, surfacing as a terminal `failed`
    /// `PairingProgress`.
    fn cancel_pair(&self, tv_id: &str) -> zbus::Result<()>;

    /// Get the admin baseline for user profiles as JSON.
    fn get_user_profile_default(&self) -> zbus::Result<String>;

    /// Get all user profiles as (name, json) pairs.
    fn list_user_profiles(&self) -> zbus::Result<Vec<(String, String)>>;

    /// JSON-shape system-profile catalog. Returns `(name, json)` pairs
    /// where the JSON serializes a `SystemProfileConfig`. Bound to the
    /// `ListSystemProfilesJson` wire slot — the `ListSystemProfiles`
    /// wire name carries the property-bag `a{sa{sv}}` shape.
    #[zbus(name = "ListSystemProfilesJson")]
    fn list_system_profiles(&self) -> zbus::Result<Vec<(String, String)>>;

    // ---- System-Profile property-bag CRUD surface ----------------------

    /// Catalog list: every system profile keyed by UUID string.
    /// Wire shape: `a{sa{sv}}` (uuid → body).
    #[zbus(name = "ListSystemProfiles")]
    fn list_system_profile_bodies(&self) -> zbus::Result<HashMap<String, SystemProfileBody>>;

    /// Fetch one body by UUID string. Malformed or absent UUIDs
    /// return `org.tisvlay.Error.ProfileNotFound`.
    fn get_system_profile(&self, uuid: &str) -> zbus::Result<SystemProfileBody>;

    /// Atomic full-body replace. Renames fold into this method —
    /// the new name lives on `body.name`.
    fn update_system_profile(&self, uuid: &str, body: SystemProfileBody) -> zbus::Result<()>;

    /// Mint a new UUID under the given display name. Empty / reserved
    /// / duplicate names are rejected with
    /// `org.tisvlay.Error.ProfileNameInvalid`. Returns the new UUID
    /// as a string.
    fn create_system_profile(&self, name: &str) -> zbus::Result<String>;

    /// Clone an existing body under a new UUID + name. Returns the
    /// new UUID as a string.
    fn duplicate_system_profile(&self, source_uuid: &str, new_name: &str) -> zbus::Result<String>;

    /// Delete a profile. Rejects the reserved default UUID
    /// (`org.tisvlay.Error.ReservedProfile`) and rejects if at least
    /// one TV references the target
    /// (`org.tisvlay.Error.ProfileReferenced`).
    fn delete_system_profile(&self, uuid: &str) -> zbus::Result<()>;

    /// Hard-coded baseline body — every profile inherits from this
    /// when its fields are all `None`. Static.
    fn get_resolved_system_profile_defaults(&self) -> zbus::Result<SystemProfileBody>;

    /// Per-TV resolved walk: returns the fully-resolved body for
    /// the named TV's assigned profile (per-TV body → default body
    /// → hardcoded baseline).
    fn get_effective_system_profile(&self, tv_id: &str) -> zbus::Result<SystemProfileBody>;

    /// A system profile's body was mutated (any field including
    /// `name`). Clients with a cached body should re-fetch via
    /// `GetSystemProfile(uuid)`.
    #[zbus(signal)]
    fn system_profile_changed(&self, uuid: String) -> zbus::Result<()>;

    /// A new system profile appeared in the catalog (Create or
    /// Duplicate).
    #[zbus(signal)]
    fn system_profile_added(&self, uuid: String) -> zbus::Result<()>;

    /// A system profile was removed from the catalog.
    #[zbus(signal)]
    fn system_profile_removed(&self, uuid: String) -> zbus::Result<()>;

    /// Pairing-flow phase transition. Phase strings: `awaiting_tv`,
    /// `awaiting_input`, `complete`, `failed`. See
    /// [`tisvlay_core::PairingState`] for the canonical enum.
    #[zbus(signal)]
    fn pairing_progress(&self, tv_id: String, phase: String, message: String) -> zbus::Result<()>;

    /// One TV was discovered during a `DiscoverTvs` scan. Tuple shape
    /// matches the synchronous RPC return:
    /// `(id, name, address, (manufacturer, model, serial), vendor,
    /// vendor_data)`.
    #[zbus(signal)]
    fn discovery_found(
        &self,
        id: String,
        name: String,
        address: String,
        hardware_id: (String, String, String),
        vendor: String,
        vendor_data: std::collections::HashMap<String, String>,
    ) -> zbus::Result<()>;
}

// =============================================================================
// TV Base Interface Proxy
// =============================================================================

/// D-Bus proxy for the base TV interface.
#[proxy(interface = "org.tisvlay.Tv1", default_service = "org.tisvlay.Daemon")]
trait TisvlayTv {
    /// TV UUID (RFC 4122). Stable, opaque handle generated at pair time.
    #[zbus(property)]
    fn uuid(&self) -> zbus::Result<String>;

    /// TV display name.
    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;

    /// Set TV display name.
    #[zbus(property)]
    fn set_name(&self, name: &str) -> zbus::Result<()>;

    /// TV vendor ID.
    #[zbus(property)]
    fn vendor(&self) -> zbus::Result<String>;

    /// Whether the TV is currently connected.
    #[zbus(property)]
    fn connected(&self) -> zbus::Result<bool>;

    /// Whether this TV participates in automated management.
    #[zbus(property)]
    fn managed(&self) -> zbus::Result<bool>;

    /// Set whether this TV participates in automated management.
    #[zbus(property)]
    fn set_managed(&self, managed: bool) -> zbus::Result<()>;

    /// System profile UUID assigned to this TV (string-formatted).
    #[zbus(property)]
    fn system_profile(&self) -> zbus::Result<String>;

    /// Set the system profile UUID for this TV.
    #[zbus(property)]
    fn set_system_profile(&self, profile_uuid: &str) -> zbus::Result<()>;

    /// The HDMI input the controlling PC is plugged into. Drives
    /// `Tv1.Automated` recomputation.
    #[zbus(property)]
    fn primary_input(&self) -> zbus::Result<String>;

    /// Set the primary input for this TV.
    #[zbus(property)]
    fn set_primary_input(&self, input: &str) -> zbus::Result<()>;

    /// Vendor-reported hardware identity: `(manufacturer, model, serial)`.
    /// Empty strings represent unknown fields.
    #[zbus(property)]
    fn hardware_id(&self) -> zbus::Result<(String, String, String)>;

    /// User-editable vendor-config rows (LG: IP, MAC). Drives the
    /// GUI's CONFIG group view-mode display and matches the field
    /// set the edit form mutates via `Tv1.UpdateConfig`. Backends
    /// populate this via `TvConnector::display_config` —
    /// `UserInput`-visibility schema fields only.
    #[zbus(property)]
    fn vendor_config(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Backend-discovered diagnostic rows (LG: WebSocket scheme).
    /// Read-only — surfaced in the GUI's INFO group alongside
    /// hardware identity. Backends populate this via
    /// `TvConnector::display_diagnostics` —
    /// `Diagnostic`-visibility schema fields only.
    #[zbus(property)]
    fn vendor_info(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Capabilities supported by this TV.
    #[zbus(property)]
    fn capabilities(&self) -> zbus::Result<Vec<String>>;

    /// Force reconnection.
    fn reconnect(&self) -> zbus::Result<()>;

    /// Re-pair this TV: refresh credentials in place without losing
    /// top-level config (name, profile, managed, primary_input).
    /// Returns immediately after sync validation; the handshake
    /// runs in a daemon-side task. Outcome surfaces via
    /// `Manager1.PairingProgress` filtered on this TV's `tv_id` to
    /// a terminal `complete` / `failed` phase — drain via
    /// `DaemonClient::drain_pair_to_terminal`. Cancel an in-flight
    /// re-pair via `Manager1.CancelPair(tv_id)`.
    fn re_pair(&self) -> zbus::Result<()>;

    /// Remove this TV from the daemon's configuration.
    ///
    /// Drops the in-memory entry, deletes from the config file,
    /// and unregisters every `Tv1.*` interface at this path so
    /// `ObjectManager.InterfacesRemoved` fires for watchers.
    fn remove(&self) -> zbus::Result<()>;

    /// Return the current TV config as a JSON string for prefilling
    /// the edit pane. Top-level fields + non-`Secret` vendor fields.
    fn get_config(&self) -> zbus::Result<String>;

    /// Apply a JSON patch to this TV's config atomically.
    /// Server-side schema-validated. Vendor changes trigger a
    /// reconnect.
    fn update_config(&self, json_patch: &str) -> zbus::Result<()>;
}

// =============================================================================
// TV Feature Interface Proxies
// =============================================================================

/// D-Bus proxy for TV power control.
#[proxy(
    interface = "org.tisvlay.Tv1.Power",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvPower {
    /// Power on the TV (Wake-on-LAN).
    fn power_on(&self) -> zbus::Result<()>;

    /// Power off the TV.
    fn power_off(&self) -> zbus::Result<()>;

    /// Get current power state.
    #[zbus(property)]
    fn power_state(&self) -> zbus::Result<String>;

    /// Whether the vendor backend can wake the TV from standby.
    /// Surfaces enough info for clients to gate the Power-on button
    /// on `Connected || SupportsWake` — true means there's at least
    /// one viable path to bring the TV online.
    #[zbus(property)]
    fn supports_wake(&self) -> zbus::Result<bool>;

    /// Phase transitions during a power-on flow.
    ///
    /// Phase string is one of `wol_sent`, `direct_connect`,
    /// `connecting`, `connected`, `failed` — see
    /// [`crate::PowerCyclePhase`] for the typed shape.
    #[zbus(signal)]
    fn power_cycle_progress(&self, phase: String) -> zbus::Result<()>;
}

/// D-Bus proxy for TV screen control.
#[proxy(
    interface = "org.tisvlay.Tv1.Screen",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvScreen {
    /// Whether screen is on.
    #[zbus(property)]
    fn is_screen_on(&self) -> zbus::Result<bool>;

    /// Set screen on/off (true = unblank, false = blank).
    fn set_screen(&self, on: bool) -> zbus::Result<()>;
}

/// D-Bus proxy for TV volume control.
#[proxy(
    interface = "org.tisvlay.Tv1.Volume",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvVolume {
    /// Current volume level (0-100).
    #[zbus(property)]
    fn volume(&self) -> zbus::Result<u8>;

    /// Set volume level (0-100).
    fn set_volume(&self, level: u8) -> zbus::Result<()>;

    /// Whether audio is currently muted.
    #[zbus(property)]
    fn muted(&self) -> zbus::Result<bool>;

    /// Set mute state.
    fn set_muted(&self, muted: bool) -> zbus::Result<()>;
}

/// D-Bus proxy for TV input control.
#[proxy(
    interface = "org.tisvlay.Tv1.Input",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvInput {
    /// Switch to input.
    fn switch_input(&self, input_id: &str) -> zbus::Result<()>;

    /// Current input ID.
    #[zbus(property)]
    fn current_input(&self) -> zbus::Result<String>;

    /// Available inputs as (id, label) tuples.
    #[zbus(property)]
    fn available_inputs(&self) -> zbus::Result<Vec<(String, String)>>;
}

/// D-Bus proxy for TV backlight control.
#[proxy(
    interface = "org.tisvlay.Tv1.Backlight",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvBacklight {
    /// Current backlight level (0-100).
    #[zbus(property)]
    fn backlight(&self) -> zbus::Result<u8>;

    /// Set backlight level (0-100).
    fn set_backlight(&self, level: u8) -> zbus::Result<()>;
}

// =============================================================================
// TV State Change (for signal subscription)
// =============================================================================

/// A state change event received from the daemon via D-Bus signals.
#[derive(Debug, Clone)]
pub enum TvStateChange {
    /// One or more properties changed on a TV (volume, power, connected, etc.).
    ///
    /// Triggered by `org.freedesktop.DBus.Properties.PropertiesChanged` signals
    /// on per-TV objects. The subscriber should re-read the TV's properties or
    /// do a full state refresh.
    PropertiesChanged { tv_id: String },
    /// TV list structure changed (add/remove) -- requires full refresh.
    TvListChanged,
}

// =============================================================================
// D-Bus Signal Subscription Helpers
// =============================================================================

/// Add a D-Bus signal match rule on the bus.
///
/// Tells the bus daemon to deliver signals matching the given rule to
/// this connection. Use [`subscribe_signals`] if you also want a
/// `MessageStream` for receiving them.
pub async fn add_signal_match(connection: &zbus::Connection, match_rule: &str) -> Result<()> {
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &match_rule,
        )
        .await
        .with_context(|| format!("Failed to add match rule: {}", match_rule))?;
    Ok(())
}

/// Add a D-Bus signal match rule and return a `MessageStream`.
///
/// Combines [`add_signal_match`] with a `MessageStream` so the caller
/// can immediately start receiving matching signals. Apply `filter_map`
/// to parse the signals you care about.
pub async fn subscribe_signals(
    connection: &zbus::Connection,
    match_rule: &str,
) -> Result<zbus::MessageStream> {
    add_signal_match(connection, match_rule).await?;
    Ok(zbus::MessageStream::from(connection.clone()))
}

// =============================================================================
// D-Bus Service Discovery
// =============================================================================

/// Event indicating a D-Bus service appeared or disappeared.
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// The service name was acquired by a process.
    Appeared,
    /// The service name was released.
    Disappeared,
}

/// Watch for a D-Bus well-known name to appear or disappear on the bus.
///
/// Uses the standard `org.freedesktop.DBus.NameOwnerChanged` signal to
/// detect when a service registers or unregisters its name. This is the
/// idiomatic D-Bus mechanism for service discovery — no polling needed.
///
/// Returns a stream of [`ServiceEvent`]s. The stream will yield
/// `Appeared` when the name is acquired and `Disappeared` when released.
///
/// # Example
///
/// ```ignore
/// let conn = zbus::Connection::session().await?;
/// let mut stream = watch_service(&conn, "org.tisvlay.Session").await?;
/// while let Some(event) = stream.next().await {
///     match event {
///         ServiceEvent::Appeared => { /* connect to service */ }
///         ServiceEvent::Disappeared => { /* clear cached client */ }
///     }
/// }
/// ```
pub async fn watch_service(
    connection: &zbus::Connection,
    name: &str,
) -> Result<impl futures_util::Stream<Item = ServiceEvent> + use<>> {
    use futures_util::StreamExt;

    let proxy = zbus::fdo::DBusProxy::new(connection)
        .await
        .context("Failed to create DBus proxy for service discovery")?;

    let stream = proxy
        .receive_name_owner_changed_with_args(&[(0, name)])
        .await
        .context("Failed to subscribe to NameOwnerChanged")?;

    Ok(stream.filter_map(|signal| async move {
        let args = signal.args().ok()?;
        if args.new_owner().as_ref().is_some() {
            Some(ServiceEvent::Appeared)
        } else {
            Some(ServiceEvent::Disappeared)
        }
    }))
}

// =============================================================================
// TV Info Struct
// =============================================================================

/// Information about a configured TV, including cached state.
///
/// All fields are populated from a single `GetManagedObjects` call.
/// State fields reflect the daemon's cached values (updated by backend
/// subscription pushes). For disconnected TVs, state fields contain
/// defaults (power_state="unknown", volume=0, etc.).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TvInfo {
    /// TV UUID (RFC 4122). Stable opaque handle generated at pair time.
    pub uuid: String,
    /// Display name.
    pub name: String,
    /// Vendor ID.
    pub vendor: String,
    /// Whether connected.
    pub connected: bool,
    /// Whether this TV participates in automated management.
    pub managed: bool,
    /// Whether automation should fire for this TV right now.
    ///
    /// Computed from `managed`, `automate_when`, `primary_input`, and
    /// the TV's current input.
    pub automated: bool,
    /// System profile UUID assigned to this TV (string-formatted).
    pub system_profile: String,
    /// Primary-input ID for this TV (the HDMI port the controlling PC
    /// is plugged into). Empty when unset; "_unknown_" when set to
    /// the wildcard (every input is treated as primary).
    pub primary_input: String,
    /// Vendor-reported hardware identity (manufacturer / model / serial).
    pub hardware_id: crate::HardwareId,
    /// User-editable vendor-config rows (LG: IP, MAC). Mirrors
    /// `Tv1.VendorConfig`. The GUI's CONFIG group renders these
    /// in view mode and edits the same fields via the schema
    /// form when the user clicks the edit pencil.
    pub vendor_config: Vec<(String, String)>,
    /// Backend-discovered diagnostic rows (LG: WebSocket scheme).
    /// Mirrors `Tv1.VendorInfo`. Read-only — refreshed only on
    /// re-pair. Rendered in the GUI's INFO group alongside the
    /// hardware identity columns.
    pub vendor_info: Vec<(String, String)>,
    /// Desktop-side identity from the daemon's in-process DRM-EDID
    /// probe. Empty strings until `tisvlayd` matches a connector for
    /// this TV (typically on the first `ConnectionChanged(true)`).
    pub desktop: crate::DesktopMapping,
    /// D-Bus object path.
    pub path: String,

    // State from sub-interface properties
    /// Power state (e.g., "active", "standby", "screen_off", "unknown").
    pub power_state: String,
    /// Whether the vendor backend can wake the TV from standby.
    /// Surfaced from `Tv1.Power.SupportsWake` so GUIs gate the
    /// Power-on button on `connected || supports_wake`.
    pub supports_wake: bool,
    /// Whether the screen is on (not blanked).
    pub is_screen_on: bool,
    /// Current volume level (0-100).
    pub volume: u8,
    /// Whether audio is muted.
    pub muted: bool,
    /// Currently active input ID (empty if unknown).
    pub current_input: String,
    /// Current backlight level (0-100).
    pub backlight: u8,
    /// Available inputs as (id, label) pairs.
    pub inputs: Vec<(String, String)>,
}

// =============================================================================
// TV Proxy Cache
// =============================================================================

/// Cached proxies for a single TV.
struct TvProxies {
    base: TisvlayTvProxy<'static>,
    power: TisvlayTvPowerProxy<'static>,
    screen: TisvlayTvScreenProxy<'static>,
    volume: TisvlayTvVolumeProxy<'static>,
    input: TisvlayTvInputProxy<'static>,
    backlight: TisvlayTvBacklightProxy<'static>,
}

impl TvProxies {
    async fn new(connection: zbus::Connection, path: OwnedObjectPath) -> Result<Self> {
        let base = TisvlayTvProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(path.clone())?
            .build()
            .await
            .context("Failed to create TV base proxy")?;

        let power = TisvlayTvPowerProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(path.clone())?
            .build()
            .await
            .context("Failed to create TV power proxy")?;

        let screen = TisvlayTvScreenProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(path.clone())?
            .build()
            .await
            .context("Failed to create TV screen proxy")?;

        let volume = TisvlayTvVolumeProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(path.clone())?
            .build()
            .await
            .context("Failed to create TV volume proxy")?;

        let input = TisvlayTvInputProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(path.clone())?
            .build()
            .await
            .context("Failed to create TV input proxy")?;

        let backlight = TisvlayTvBacklightProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(path)?
            .build()
            .await
            .context("Failed to create TV backlight proxy")?;

        Ok(Self {
            base,
            power,
            screen,
            volume,
            input,
            backlight,
        })
    }
}

// =============================================================================
// Daemon Client
// =============================================================================

/// Client for communicating with the tisvlay system daemon via D-Bus.
///
/// This client provides a high-level interface for all TV control operations.
/// It automatically handles D-Bus connection setup and error conversion.
#[derive(Clone)]
pub struct DaemonClient {
    connection: zbus::Connection,
    manager: TisvlayManagerProxy<'static>,
    object_manager: ObjectManagerProxy<'static>,
    tv_proxies: Arc<RwLock<HashMap<String, Arc<TvProxies>>>>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient").finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Create a new daemon client connected to the system bus.
    ///
    /// Use this for production deployments where `tisvlayd` runs as a system service.
    pub async fn new_system() -> Result<Self> {
        let connection = zbus::Connection::system()
            .await
            .context("Failed to connect to system D-Bus")?;

        Self::from_connection(connection).await
    }

    /// Create a new daemon client connected to the session bus.
    ///
    /// Use this for development when running `tisvlayd --session`.
    pub async fn new_session() -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;

        Self::from_connection(connection).await
    }

    /// Create a client from an existing D-Bus connection.
    ///
    /// Use this when the connection has been built explicitly (e.g.,
    /// via [`zbus::connection::Builder::address`] in tests pointing at
    /// a private `dbus-daemon`). Going through `Builder::address`
    /// bypasses the per-process `OnceCell` cache that
    /// [`zbus::Connection::session`] / `system` use, so each test gets
    /// an isolated connection.
    pub async fn from_connection(connection: zbus::Connection) -> Result<Self> {
        let manager = TisvlayManagerProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(MANAGER_PATH)?
            .build()
            .await
            .context("Failed to create D-Bus proxy for tisvlay manager")?;

        let object_manager = ObjectManagerProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(OBJECT_MANAGER_PATH)?
            .build()
            .await
            .context("Failed to create D-Bus ObjectManager proxy")?;

        Ok(Self {
            connection,
            manager,
            object_manager,
            tv_proxies: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Access the underlying D-Bus connection.
    ///
    /// Useful for callers that need to set up additional signal watches
    /// on the same bus (e.g., [`StateWatcher`]).
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }

    /// Get or create proxies for a TV.
    async fn get_tv_proxies(&self, tv_id: &str) -> Result<()> {
        // Use a single write lock to avoid TOCTOU race where two concurrent
        // callers could both miss the read check and create duplicate proxies.
        let mut cache = self.tv_proxies.write().await;
        if cache.contains_key(tv_id) {
            return Ok(());
        }

        let path = tv_path(tv_id);
        let obj_path = OwnedObjectPath::try_from(path).context("Invalid TV path")?;
        let proxies = TvProxies::new(self.connection.clone(), obj_path).await?;
        cache.insert(tv_id.to_string(), Arc::new(proxies));

        Ok(())
    }

    /// Check if the daemon is available and responding.
    pub async fn is_available(&self) -> bool {
        self.object_manager.get_managed_objects().await.is_ok()
    }

    /// Compile-time version of the running daemon binary. Returns the
    /// `Manager1.Version` const property — surfaces installed-version
    /// drift between the calling process (e.g., the GUI) and the
    /// daemon after a partial upgrade where services weren't restarted.
    pub async fn version(&self) -> Result<String> {
        self.manager
            .version()
            .await
            .context("Failed to read daemon version")
    }

    /// Resolved on-disk path of the daemon's config file. Returns the
    /// `Manager1.ConfigPath` const property — lets the About-pane
    /// COMPONENTS row show the actual path rather than a hardcoded
    /// guess regardless of `--config` overrides.
    pub async fn config_path(&self) -> Result<String> {
        self.manager
            .config_path()
            .await
            .context("Failed to read daemon config path")
    }

    /// Execute an operation on a TV's cached proxies, handling proxy lookup boilerplate.
    ///
    /// This helper ensures the proxy cache is populated for the given TV, acquires a
    /// read lock, looks up the proxies, and delegates to the provided closure. The `op`
    /// string is used for tracing and error context (e.g., "turn screen off").
    ///
    /// The closure receives `&TvProxies` and must return a boxed future to erase the
    /// borrow lifetime (proxy method futures borrow from `&self`).
    async fn with_proxies<T, F>(&self, tv_id: &str, op: &str, f: F) -> Result<T>
    where
        T: 'static,
        F: for<'a> FnOnce(
            &'a TvProxies,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = zbus::Result<T>> + Send + 'a>,
        >,
    {
        debug!(tv_id = %tv_id, op = op, "D-Bus call");
        self.get_tv_proxies(tv_id).await?;
        // Extract the Arc and release the lock before the async D-Bus call so
        // a concurrent `reload_config` (write lock) is not blocked by in-flight calls.
        let proxies = {
            let cache = self.tv_proxies.read().await;
            Arc::clone(
                cache
                    .get(tv_id)
                    .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?,
            )
        };
        f(&proxies)
            .await
            .with_context(|| format!("Failed to {} for TV: {}", op, tv_id))
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// List all configured TVs.
    ///
    /// Uses `org.freedesktop.DBus.ObjectManager.GetManagedObjects` — a single
    /// round-trip that returns all object paths and their interface properties at once,
    /// replacing the previous 1 + 6N round-trips (one `GetTvs` + six property fetches per TV).
    pub async fn list_tvs(&self) -> Result<Vec<TvInfo>> {
        let managed_objects = self
            .object_manager
            .get_managed_objects()
            .await
            .context("Failed to get managed objects from daemon")?;

        let mut tvs = Vec::new();

        for (path, ifaces) in managed_objects {
            // Only consider objects that expose the base TV interface.
            let Some(props) = ifaces
                .iter()
                .find(|(k, _)| k.as_str() == "org.tisvlay.Tv1")
                .map(|(_, v)| v)
            else {
                continue;
            };

            // Track which properties fell back to defaults so we can emit a single
            // diagnostic per entry rather than one log line per property.
            let fallbacks = std::cell::RefCell::new(Vec::<String>::new());

            let get_str = |key: &str| -> String {
                props
                    .get(key)
                    .and_then(|v: &OwnedValue| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| {
                        fallbacks.borrow_mut().push(key.to_owned());
                        String::new()
                    })
            };
            let get_bool = |key: &str, default: bool| -> bool {
                props
                    .get(key)
                    .and_then(|v: &OwnedValue| bool::try_from(v.clone()).ok())
                    .unwrap_or_else(|| {
                        fallbacks.borrow_mut().push(key.to_owned());
                        default
                    })
            };

            let uuid = get_str("Uuid");
            let name = get_str("Name");
            let vendor = get_str("Vendor");
            let connected = get_bool("Connected", false);
            let managed = get_bool("Managed", true);
            let automated = get_bool("Automated", true);
            let system_profile = {
                let sp = get_str("SystemProfile");
                if sp.is_empty() {
                    crate::DEFAULT_SYSTEM_PROFILE_UUID.to_string()
                } else {
                    sp
                }
            };

            let primary_input = get_str("PrimaryInput");

            let hardware_id = props
                .get("HardwareId")
                .and_then(|v: &OwnedValue| <(String, String, String)>::try_from(v.clone()).ok())
                .map(crate::HardwareId::from_dbus_tuple)
                .unwrap_or_default();

            let vendor_config: Vec<(String, String)> = props
                .get("VendorConfig")
                .and_then(|v: &OwnedValue| <Vec<(String, String)>>::try_from(v.clone()).ok())
                .unwrap_or_default();
            let vendor_info: Vec<(String, String)> = props
                .get("VendorInfo")
                .and_then(|v: &OwnedValue| <Vec<(String, String)>>::try_from(v.clone()).ok())
                .unwrap_or_default();

            // Helper to extract properties from a sub-interface.
            let get_iface_props = |iface_name: &str| -> Option<&HashMap<String, OwnedValue>> {
                ifaces
                    .iter()
                    .find(|(k, _)| k.as_str() == iface_name)
                    .map(|(_, v)| v)
            };

            let get_sub_str = |iface: &str, key: &str, default: &str| -> String {
                get_iface_props(iface)
                    .and_then(|p| p.get(key))
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_else(|| default.to_string())
            };
            let get_sub_bool = |iface: &str, key: &str, default: bool| -> bool {
                get_iface_props(iface)
                    .and_then(|p| p.get(key))
                    .and_then(|v| bool::try_from(v.clone()).ok())
                    .unwrap_or(default)
            };
            let get_sub_u8 = |iface: &str, key: &str| -> u8 {
                get_iface_props(iface)
                    .and_then(|p| p.get(key))
                    .and_then(|v| u8::try_from(v.clone()).ok())
                    .unwrap_or(0)
            };

            // Extract state from sub-interfaces
            let power_state = get_sub_str("org.tisvlay.Tv1.Power", "PowerState", "unknown");
            let supports_wake = get_sub_bool("org.tisvlay.Tv1.Power", "SupportsWake", false);
            let is_screen_on = get_sub_bool("org.tisvlay.Tv1.Screen", "IsScreenOn", true);
            let volume = get_sub_u8("org.tisvlay.Tv1.Volume", "Volume");
            let muted = get_sub_bool("org.tisvlay.Tv1.Volume", "Muted", false);
            let current_input = get_sub_str("org.tisvlay.Tv1.Input", "CurrentInput", "");
            let backlight = get_sub_u8("org.tisvlay.Tv1.Backlight", "Backlight");
            let inputs: Vec<(String, String)> = get_iface_props("org.tisvlay.Tv1.Input")
                .and_then(|p| p.get("AvailableInputs"))
                .and_then(|v| <Vec<(String, String)>>::try_from(v.clone()).ok())
                .unwrap_or_default();
            let desktop = crate::DesktopMapping {
                connector: get_sub_str("org.tisvlay.Tv1.Desktop", "Connector", ""),
            };

            let fallbacks = fallbacks.into_inner();
            if !fallbacks.is_empty() {
                debug!(
                    path = %path,
                    ?fallbacks,
                    "TV object has missing or invalid properties; using defaults"
                );
            }

            if uuid.is_empty() {
                warn!(path = %path, "TV object returned by daemon has empty Uuid, skipping");
                continue;
            }

            tvs.push(TvInfo {
                uuid,
                name,
                vendor,
                connected,
                managed,
                automated,
                system_profile,
                primary_input,
                hardware_id,
                vendor_config,
                vendor_info,
                desktop,
                path: path.as_str().to_string(),
                power_state,
                supports_wake,
                is_screen_on,
                volume,
                muted,
                current_input,
                backlight,
                inputs,
            });
        }

        Ok(tvs)
    }

    /// Discover TVs on the network.
    ///
    /// Returns `(id, name, address, hardware_id, vendor, vendor_data)`
    /// tuples. `vendor_data` carries free-form key/value entries the
    /// daemon collected during discovery (e.g. `"mac"` from the kernel
    /// neighbour table for IP-addressed backends); wizards match these
    /// against each vendor's `PairingField::autofill` directives.
    pub async fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> Result<
        Vec<(
            String,
            String,
            String,
            crate::HardwareId,
            String,
            std::collections::HashMap<String, String>,
        )>,
    > {
        let raw = self
            .manager
            .discover_tvs(timeout_ms)
            .await
            .context("Failed to discover TVs")?;
        Ok(raw
            .into_iter()
            .map(|(id, name, address, hw_tuple, vendor, vendor_data)| {
                (
                    id,
                    name,
                    address,
                    crate::HardwareId::from_dbus_tuple(hw_tuple),
                    vendor,
                    vendor_data,
                )
            })
            .collect())
    }

    /// Start a streaming discovery session.
    ///
    /// Pair with [`discovery_found_stream`](Self::discovery_found_stream)
    /// to drain results. `timeout_ms = 0` runs until
    /// [`stop_discovery`](Self::stop_discovery) is called (subject to
    /// the daemon's 5-minute hard cap).
    pub async fn start_discovery(&self, timeout_ms: u32) -> Result<String> {
        self.manager
            .start_discovery(timeout_ms)
            .await
            .context("Failed to start discovery session")
    }

    /// Stop a streaming discovery session. Idempotent.
    pub async fn stop_discovery(&self, session_id: &str) -> Result<()> {
        self.manager
            .stop_discovery(session_id)
            .await
            .context("Failed to stop discovery session")
    }

    /// Enrich a user-typed manual-entry (vendor, address) for the
    /// pairing wizard's MAC autofill story. Returns the same
    /// `vendor_data`-shaped map that the discovered-TV path
    /// produces; empty when nothing can be auto-detected.
    pub async fn enrich_manual_entry(
        &self,
        vendor: &str,
        address: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        self.manager
            .enrich_manual_entry(vendor, address)
            .await
            .context("Failed to enrich manual entry")
    }

    /// Get supported vendors.
    ///
    /// Returns a vector of tuples: `(vendor_id, vendor_name)`.
    pub async fn get_supported_vendors(&self) -> Result<Vec<(String, String)>> {
        self.manager
            .get_supported_vendors()
            .await
            .context("Failed to get supported vendors")
    }

    /// Fetch one vendor's `Capabilities` bitflag, parsed back to the
    /// typed shape. Unknown vendor → `Capabilities::empty()` (the
    /// wire layer returns an empty list rather than erroring; the
    /// typed wrapper preserves that semantics so the GUI can treat
    /// missing caps as "feature unavailable").
    pub async fn get_vendor_capabilities(&self, vendor: &str) -> Result<crate::Capabilities> {
        let strings = self
            .manager
            .get_vendor_capabilities(vendor)
            .await
            .context("Failed to get vendor capabilities")?;
        let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
        Ok(crate::Capabilities::from_strings(&refs))
    }

    /// Get the pairing-form schema for a vendor.
    ///
    /// Returns the typed [`PairingField`] list the wizard should
    /// render. The daemon serializes it as JSON over D-Bus
    /// (matching the `config_json` convention on `AddTv`); this
    /// wrapper deserializes back into typed Rust on the client.
    pub async fn get_pairing_schema(&self, vendor: &str) -> Result<Vec<crate::PairingField>> {
        let json = self
            .manager
            .get_pairing_schema(vendor)
            .await
            .context("Failed to get pairing schema")?;
        serde_json::from_str(&json).context("Failed to parse pairing schema JSON")
    }

    /// Pair/add a new TV.
    ///
    /// This connects to the TV and triggers a pairing prompt on the TV screen.
    /// The user must accept the prompt for pairing to succeed.
    ///
    /// `config_json` contains vendor-specific connection parameters as JSON
    /// (e.g., `{"ip": "192.168.1.100", "mac": "AA:BB:CC:DD:EE:FF"}` for LG).
    ///
    /// Returns the TV ID assigned to the new TV.
    pub async fn pair_tv(
        &self,
        name: &str,
        vendor: &str,
        config_json: &str,
        managed: bool,
    ) -> Result<String> {
        debug!(name = %name, vendor = %vendor, managed = %managed, "Pairing TV via daemon");
        // AddTv returns (object_path, tv_id). We use the canonical TV ID
        // directly — never derive an ID from the D-Bus path, since the
        // path sanitizer is lossy (see docs/bugs.md).
        let (_path, tv_id) = self
            .manager
            .add_tv(name, vendor, config_json, managed)
            .await
            .with_context(|| format!("Failed to pair TV '{}'", name))?;

        Ok(tv_id)
    }

    /// Remove a TV from configuration.
    ///
    /// Routes through the per-object `Tv1.Remove()` method so the
    /// daemon's handler has `&self.tv_id` directly — no path
    /// decoding, no hyphen/underscore mangling. The daemon
    /// unregisters every `Tv1.*` interface at this path as part
    /// of the call; we drop the proxy cache entry on success.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "remove", |p| Box::pin(p.base.remove()))
            .await?;

        let mut cache = self.tv_proxies.write().await;
        cache.remove(tv_id);
        Ok(())
    }

    /// Re-pair the TV: refresh credentials in place without losing
    /// top-level config. Returns immediately after sync validation;
    /// outcome surfaces via `Manager1.PairingProgress`. Use
    /// [`Self::drain_pair_to_terminal`] to await a terminal phase.
    /// Cancel via [`Self::cancel_pair`].
    pub async fn re_pair(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "re_pair", |p| Box::pin(p.base.re_pair()))
            .await
    }

    /// Cancel an in-flight pairing handshake for the given TV.
    ///
    /// Works for both AddTv-in-flight and re-pair-in-flight. Returns
    /// `Err(NotPairing)` (carried via the underlying zbus error
    /// surface as `org.tisvlay.Error.NotPairing`) if no in-flight
    /// session exists for the tv_id.
    pub async fn cancel_pair(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling CancelPair on daemon");
        self.manager
            .cancel_pair(tv_id)
            .await
            .context("Failed to cancel pair")?;
        Ok(())
    }

    /// Fetch a TV's config as JSON (top-level + non-`Secret` vendor
    /// fields). Used by the GUI's edit pane to prefill its form.
    pub async fn get_tv_config(&self, tv_id: &str) -> Result<String> {
        self.with_proxies(tv_id, "get config", |p| Box::pin(p.base.get_config()))
            .await
    }

    /// Apply a JSON patch atomically to a TV's config. Routes
    /// through the per-object `Tv1.UpdateConfig` method; the
    /// daemon validates each field server-side against the
    /// vendor's `config_schema`, persists to disk, and triggers a
    /// reconnect if any vendor field changed.
    pub async fn update_tv_config(&self, tv_id: &str, json_patch: &str) -> Result<()> {
        let patch = json_patch.to_string();
        self.with_proxies(tv_id, "update config", move |p| {
            let patch = patch.clone();
            Box::pin(async move { p.base.update_config(&patch).await })
        })
        .await
    }

    /// Subscribe to structural changes in the daemon's TV list.
    ///
    /// Returns a stream that yields `()` whenever the daemon's
    /// `ObjectManager` emits `InterfacesAdded` or `InterfacesRemoved`
    /// — i.e. whenever a TV is paired or removed. The stream
    /// intentionally does not carry diff information so subscribers
    /// always reconcile against the daemon's authoritative
    /// `list_tvs()` rather than against a wire-format payload that
    /// could drift from the underlying state.
    ///
    /// Used by `tisvlay-session` to prune orphan `[[tv]]` profile
    /// assignments from user config when the corresponding TV is
    /// unpaired.
    pub async fn tv_list_change_stream(&self) -> Result<impl Stream<Item = ()> + Send + 'static> {
        let rule = format!(
            "type='signal',sender='{}',interface='org.freedesktop.DBus.ObjectManager',path='{}'",
            DAEMON_SERVICE, OBJECT_MANAGER_PATH,
        );
        let raw = subscribe_signals(&self.connection, &rule)
            .await
            .context("Failed to subscribe to ObjectManager signals on daemon")?;
        Ok(raw.filter_map(|msg_result| async move {
            let msg = msg_result.ok()?;
            let header = msg.header();
            let member = header.member()?.to_string();
            (member == "InterfacesAdded" || member == "InterfacesRemoved").then_some(())
        }))
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    pub async fn power_on(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "power on", |p| Box::pin(p.power.power_on()))
            .await
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "power off", |p| Box::pin(p.power.power_off()))
            .await
    }

    /// Subscribe to `Tv1.Power.PowerCycleProgress` signals for a TV.
    ///
    /// Returns a stream of [`crate::PowerCyclePhase`] values yielded as
    /// the daemon walks through `WolSent` → `Connecting` → `Connected`
    /// or `Failed` during a `PowerOn`. Set up the subscription **before**
    /// calling [`Self::power_on`] — phases emitted before the stream is
    /// active are dropped.
    ///
    /// Unknown phase strings (forward-compat slack from a newer daemon)
    /// are skipped silently.
    pub async fn power_cycle_progress_stream(
        &self,
        tv_id: &str,
    ) -> Result<impl futures_util::Stream<Item = crate::PowerCyclePhase> + use<>> {
        use futures_util::StreamExt;

        let path = tv_path(tv_id);
        let obj_path = OwnedObjectPath::try_from(path).context("Invalid TV path")?;
        let proxy = TisvlayTvPowerProxy::builder(&self.connection)
            .path(obj_path)
            .context("Invalid TV path for Tv1.Power proxy")?
            .build()
            .await
            .context("failed to build Tv1.Power proxy")?;
        let raw = proxy
            .receive_power_cycle_progress()
            .await
            .context("failed to subscribe to PowerCycleProgress signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            match args.phase.as_str() {
                "wol_sent" => Some(crate::PowerCyclePhase::WolSent),
                "direct_connect" => Some(crate::PowerCyclePhase::DirectConnect),
                "connecting" => Some(crate::PowerCyclePhase::Connecting),
                "connected" => Some(crate::PowerCyclePhase::Connected),
                "failed" => Some(crate::PowerCyclePhase::Failed),
                _ => None,
            }
        }))
    }

    /// Subscribe to `Manager1.PairingProgress` signals.
    ///
    /// Returns a stream of `(tv_id, phase, message)` tuples emitted
    /// during `pair_tv()` calls. Phase strings: `awaiting_tv`,
    /// `awaiting_input`, `complete`, `failed`. Set up the stream
    /// **before** calling `pair_tv` if you want to observe its
    /// progress — phases emitted before the stream is active are
    /// dropped.
    pub async fn pairing_progress_stream(
        &self,
    ) -> Result<impl futures_util::Stream<Item = (String, String, String)> + use<>> {
        use futures_util::StreamExt;

        let raw = self
            .manager
            .receive_pairing_progress()
            .await
            .context("failed to subscribe to PairingProgress signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Some((args.tv_id, args.phase, args.message))
        }))
    }

    /// Drain a pre-subscribed `pairing_progress_stream` until a
    /// terminal phase for `target_tv_id` arrives, calling
    /// `on_progress(phase, message)` for each non-terminal event.
    ///
    /// Events with a different `tv_id` are silently dropped — the
    /// signal is bus-wide and any number of clients may be running
    /// concurrent pairings, but the caller already knows which
    /// `tv_id` is theirs (from the `pair_tv` RPC return, or the
    /// `Tv1.RePair` argument).
    ///
    /// Returns:
    /// - `Ok(PairTerminal::Complete { … })` on `complete` phase.
    /// - `Ok(PairTerminal::Failed { … })` on `failed` phase.
    /// - `Err(...)` if the stream ends without reaching a terminal
    ///   phase (e.g., daemon disconnected mid-handshake).
    ///
    /// Subscribe to the stream **before** calling `pair_tv` /
    /// `re_pair` — `PairingProgress` signals emitted before the
    /// subscription is active are dropped.
    ///
    /// One-shot variant that consumes the stream. Callers that need
    /// to combine multiple drain passes (e.g. tests that wait for an
    /// intermediate phase, trigger a side-effect, then resume) use
    /// [`Self::drain_pair_to_terminal_pinned`] with a pinned borrow.
    pub async fn drain_pair_to_terminal<S>(
        stream: S,
        target_tv_id: &str,
        on_progress: impl FnMut(&str, &str),
    ) -> Result<PairTerminal>
    where
        S: futures_util::Stream<Item = (String, String, String)>,
    {
        let mut stream = std::pin::pin!(stream);
        Self::drain_pair_to_terminal_pinned(stream.as_mut(), target_tv_id, on_progress).await
    }

    /// Pinned-borrow workhorse for [`Self::drain_pair_to_terminal`].
    /// Useful when the caller needs to keep the stream alive across
    /// multiple operations on it (e.g. wait for `awaiting_tv`, trigger
    /// `CancelPair`, then drain the remainder to `failed`).
    pub async fn drain_pair_to_terminal_pinned<S>(
        mut stream: std::pin::Pin<&mut S>,
        target_tv_id: &str,
        mut on_progress: impl FnMut(&str, &str),
    ) -> Result<PairTerminal>
    where
        S: futures_util::Stream<Item = (String, String, String)>,
    {
        use futures_util::StreamExt;
        while let Some((tv_id, phase, message)) = stream.as_mut().next().await {
            if tv_id != target_tv_id {
                continue;
            }
            match phase.as_str() {
                "complete" => return Ok(PairTerminal::Complete { tv_id, message }),
                "failed" => return Ok(PairTerminal::Failed { tv_id, message }),
                _ => on_progress(&phase, &message),
            }
        }
        anyhow::bail!(
            "PairingProgress stream ended before reaching a terminal phase for tv_id `{target_tv_id}`"
        );
    }

    /// Subscribe to `Manager1.DiscoveryFound` signals.
    ///
    /// Returns a stream of `(id, name, address, hardware_id, vendor,
    /// vendor_data)` tuples — same shape as the `discover_tvs` RPC
    /// return value. In v1 the daemon emits these in a burst right
    /// before `DiscoverTvs` returns; clients should still set up the
    /// stream before calling `discover_tvs` so future per-backend
    /// streaming arrives intact.
    pub async fn discovery_found_stream(
        &self,
    ) -> Result<
        impl futures_util::Stream<
            Item = (
                String,
                String,
                String,
                crate::HardwareId,
                String,
                std::collections::HashMap<String, String>,
            ),
        > + use<>,
    > {
        use futures_util::StreamExt;

        let raw = self
            .manager
            .receive_discovery_found()
            .await
            .context("failed to subscribe to DiscoveryFound signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Some((
                args.id,
                args.name,
                args.address,
                crate::HardwareId::from_dbus_tuple(args.hardware_id),
                args.vendor,
                args.vendor_data,
            ))
        }))
    }

    /// Turn screen on (resume from blank).
    pub async fn screen_on(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "turn screen on", |p| {
            Box::pin(p.screen.set_screen(true))
        })
        .await
    }

    /// Turn screen off (OLED-safe blank).
    pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "turn screen off", |p| {
            Box::pin(p.screen.set_screen(false))
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch to a specific input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<()> {
        let input_id = input_id.to_owned();
        self.with_proxies(tv_id, "switch input", move |p| {
            Box::pin(async move { p.input.switch_input(&input_id).await })
        })
        .await
    }

    /// Get available inputs.
    ///
    /// Returns a vector of tuples: `(id, label)`.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>> {
        self.with_proxies(tv_id, "get inputs", |p| {
            Box::pin(p.input.available_inputs())
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Audio Control
    // -------------------------------------------------------------------------

    /// Set volume (0-100).
    pub async fn set_volume(&self, tv_id: &str, volume: u8) -> Result<()> {
        self.with_proxies(tv_id, "set volume", |p| {
            Box::pin(p.volume.set_volume(volume))
        })
        .await
    }

    /// Get current volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u8> {
        self.with_proxies(tv_id, "get volume", |p| Box::pin(p.volume.volume()))
            .await
    }

    /// Set mute state.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<()> {
        self.with_proxies(tv_id, "set mute", |p| Box::pin(p.volume.set_muted(mute)))
            .await
    }

    // -------------------------------------------------------------------------
    // Picture Control
    // -------------------------------------------------------------------------

    /// Set backlight (OLED pixel brightness, 0-100).
    pub async fn set_backlight(&self, tv_id: &str, backlight: u8) -> Result<()> {
        self.with_proxies(tv_id, "set backlight", |p| {
            Box::pin(p.backlight.set_backlight(backlight))
        })
        .await
    }

    /// Get current backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u8> {
        self.with_proxies(tv_id, "get backlight", |p| {
            Box::pin(p.backlight.backlight())
        })
        .await
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV capabilities.
    pub async fn get_capabilities(&self, tv_id: &str) -> Result<Vec<String>> {
        self.with_proxies(tv_id, "get capabilities", |p| {
            Box::pin(p.base.capabilities())
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Configuration
    // -------------------------------------------------------------------------

    /// Reload configuration from disk.
    ///
    /// This causes the daemon to re-read its configuration file and update
    /// TV connections accordingly.
    pub async fn reload_config(&self) -> Result<()> {
        debug!("Calling ReloadConfig on daemon");
        self.manager
            .reload_config()
            .await
            .context("Failed to reload daemon configuration")?;

        // Clear proxy cache since TVs may have changed
        {
            let mut cache = self.tv_proxies.write().await;
            cache.clear();
        }

        Ok(())
    }

    /// Get the admin baseline for user profiles.
    ///
    /// Returns the `[profiles.default]` from the daemon's system config.
    /// Used by session services to inherit defaults when users have no config file.
    pub async fn get_user_profile_default(&self) -> Result<crate::UserProfileConfig> {
        debug!("Calling GetUserProfileDefault on daemon");
        let json = self
            .manager
            .get_user_profile_default()
            .await
            .context("Failed to get user profile default from daemon")?;
        serde_json::from_str(&json).context("Failed to parse user profile default JSON")
    }

    /// Get all user profiles.
    ///
    /// Returns all profiles defined in the daemon's system config as
    /// `(name, json)` pairs.
    pub async fn list_user_profiles(&self) -> Result<Vec<(String, String)>> {
        debug!("Calling ListUserProfiles on daemon");
        self.manager
            .list_user_profiles()
            .await
            .context("Failed to list user profiles from daemon")
    }

    /// Get all system profiles.
    ///
    /// Returns all system profiles defined in the daemon's system config as
    /// `(name, json)` pairs.
    pub async fn list_system_profiles(&self) -> Result<Vec<(String, String)>> {
        debug!("Calling ListSystemProfiles on daemon");
        self.manager
            .list_system_profiles()
            .await
            .context("Failed to list system profiles from daemon")
    }

    // -------------------------------------------------------------------------
    // System-Profile property-bag CRUD
    //
    // Mirrors the eight `Manager1` body-dict methods. Bodies round-trip
    // as `SystemProfileBody`; UUID returns parse from the wire's
    // `String` representation.
    // -------------------------------------------------------------------------

    /// Catalog list: every system profile keyed by parsed UUID. The
    /// daemon emits wire UUIDs that are always well-formed, so a parse
    /// failure here means a buggy peer; the unparsable entry is
    /// skipped with a debug log rather than aborting the whole call.
    pub async fn list_system_profile_bodies(&self) -> Result<HashMap<Uuid, SystemProfileBody>> {
        debug!("Calling ListSystemProfiles on daemon");
        let raw = self
            .manager
            .list_system_profile_bodies()
            .await
            .context("Failed to list system profile bodies from daemon")?;
        let mut out = HashMap::with_capacity(raw.len());
        for (uuid_str, body) in raw {
            match Uuid::parse_str(&uuid_str) {
                Ok(uuid) => {
                    out.insert(uuid, body);
                }
                Err(e) => {
                    warn!(uuid = %uuid_str, error = %e, "Skipping malformed UUID from ListSystemProfiles");
                }
            }
        }
        Ok(out)
    }

    /// Fetch one body by UUID. Malformed or absent UUIDs surface as
    /// `org.tisvlay.Error.ProfileNotFound`.
    pub async fn get_system_profile(&self, uuid: Uuid) -> Result<SystemProfileBody> {
        debug!(uuid = %uuid, "Calling GetSystemProfile on daemon");
        let uuid_str = uuid.to_string();
        self.manager
            .get_system_profile(&uuid_str)
            .await
            .with_context(|| format!("Failed to get system profile '{}'", uuid_str))
    }

    /// Atomic full-body replace. Includes the new name on `body.name`;
    /// rename is just an update with a fresh `name`.
    pub async fn update_system_profile(&self, uuid: Uuid, body: &SystemProfileBody) -> Result<()> {
        debug!(uuid = %uuid, name = %body.name, "Calling UpdateSystemProfile on daemon");
        let uuid_str = uuid.to_string();
        self.manager
            .update_system_profile(&uuid_str, body.clone())
            .await
            .with_context(|| format!("Failed to update system profile '{}'", uuid_str))
    }

    /// Mint a new system profile under the given display name. Returns
    /// the new UUID. Empty / reserved / duplicate names are rejected
    /// with `org.tisvlay.Error.ProfileNameInvalid`.
    pub async fn create_system_profile(&self, name: &str) -> Result<Uuid> {
        debug!(name = %name, "Calling CreateSystemProfile on daemon");
        let raw = self
            .manager
            .create_system_profile(name)
            .await
            .with_context(|| format!("Failed to create system profile '{}'", name))?;
        Uuid::parse_str(&raw).with_context(|| {
            format!(
                "Daemon returned malformed UUID '{}' from CreateSystemProfile",
                raw
            )
        })
    }

    /// Clone `source_uuid`'s body under a new UUID + name. Returns the
    /// new UUID.
    pub async fn duplicate_system_profile(
        &self,
        source_uuid: Uuid,
        new_name: &str,
    ) -> Result<Uuid> {
        debug!(source_uuid = %source_uuid, new_name = %new_name, "Calling DuplicateSystemProfile on daemon");
        let source_str = source_uuid.to_string();
        let raw = self
            .manager
            .duplicate_system_profile(&source_str, new_name)
            .await
            .with_context(|| {
                format!(
                    "Failed to duplicate system profile '{}' as '{}'",
                    source_str, new_name
                )
            })?;
        Uuid::parse_str(&raw).with_context(|| {
            format!(
                "Daemon returned malformed UUID '{}' from DuplicateSystemProfile",
                raw
            )
        })
    }

    /// Delete a profile. Rejects the reserved default UUID with
    /// `org.tisvlay.Error.ReservedProfile`; rejects when at least one
    /// TV references the target with
    /// `org.tisvlay.Error.ProfileReferenced`.
    pub async fn delete_system_profile(&self, uuid: Uuid) -> Result<()> {
        debug!(uuid = %uuid, "Calling DeleteSystemProfile on daemon");
        let uuid_str = uuid.to_string();
        self.manager
            .delete_system_profile(&uuid_str)
            .await
            .with_context(|| format!("Failed to delete system profile '{}'", uuid_str))
    }

    /// Hard-coded baseline body — the inheritance ancestor every
    /// profile falls back to when its own fields are all `None`.
    /// Static; the daemon exposes it so clients don't have to
    /// duplicate the table.
    pub async fn get_resolved_system_profile_defaults(&self) -> Result<SystemProfileBody> {
        debug!("Calling GetResolvedSystemProfileDefaults on daemon");
        self.manager
            .get_resolved_system_profile_defaults()
            .await
            .context("Failed to get resolved system-profile defaults")
    }

    /// Per-TV resolved walk: the fully-resolved body the daemon
    /// will dispatch against (per-TV body → default body → hardcoded
    /// baseline).
    pub async fn get_effective_system_profile(&self, tv_id: &str) -> Result<SystemProfileBody> {
        debug!(tv_id = %tv_id, "Calling GetEffectiveSystemProfile on daemon");
        self.manager
            .get_effective_system_profile(tv_id)
            .await
            .with_context(|| format!("Failed to get effective system profile for TV '{}'", tv_id))
    }

    /// Subscribe to `Manager1.SystemProfileChanged` signals.
    ///
    /// Emits the UUID (parsed) every time a system profile body is
    /// mutated server-side. Set up the stream **before** the mutation
    /// you want to observe — signals emitted prior to subscription
    /// are dropped.
    pub async fn system_profile_changed_stream(&self) -> Result<impl Stream<Item = Uuid> + use<>> {
        let raw = self
            .manager
            .receive_system_profile_changed()
            .await
            .context("failed to subscribe to SystemProfileChanged signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Uuid::parse_str(&args.uuid).ok()
        }))
    }

    /// Subscribe to `Manager1.SystemProfileAdded` signals.
    pub async fn system_profile_added_stream(&self) -> Result<impl Stream<Item = Uuid> + use<>> {
        let raw = self
            .manager
            .receive_system_profile_added()
            .await
            .context("failed to subscribe to SystemProfileAdded signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Uuid::parse_str(&args.uuid).ok()
        }))
    }

    /// Subscribe to `Manager1.SystemProfileRemoved` signals.
    pub async fn system_profile_removed_stream(&self) -> Result<impl Stream<Item = Uuid> + use<>> {
        let raw = self
            .manager
            .receive_system_profile_removed()
            .await
            .context("failed to subscribe to SystemProfileRemoved signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Uuid::parse_str(&args.uuid).ok()
        }))
    }

    /// Set a TV's friendly name.
    ///
    /// Routes through the per-object `Tv1.Name` writable property
    /// (`Properties.Set`). Use [`update_tv_config`](Self::update_tv_config)
    /// when changing multiple fields atomically; this single-field
    /// helper has the lighter-weight wire format for inline controls
    /// (rename in the TVs pane header, etc.).
    pub async fn set_tv_name(&self, tv_id: &str, name: &str) -> Result<()> {
        let name = name.to_string();
        self.with_proxies(tv_id, "set name", move |p| {
            let name = name.clone();
            Box::pin(async move { p.base.set_name(&name).await })
        })
        .await
    }

    /// Toggle whether a TV participates in managed automation.
    ///
    /// Routes through the per-object `Tv1.Managed` writable property.
    pub async fn set_tv_managed(&self, tv_id: &str, managed: bool) -> Result<()> {
        self.with_proxies(tv_id, "set managed", move |p| {
            Box::pin(p.base.set_managed(managed))
        })
        .await
    }

    /// Set the system profile for a TV by UUID (string-formatted).
    ///
    /// Routes through the per-object `Tv1.SystemProfile` writable
    /// property. The daemon rejects unknown or malformed UUIDs with
    /// `InvalidArgs`.
    pub async fn set_tv_system_profile(&self, tv_id: &str, profile_uuid: &str) -> Result<()> {
        let profile_uuid = profile_uuid.to_string();
        self.with_proxies(tv_id, "set system profile", move |p| {
            let profile_uuid = profile_uuid.clone();
            Box::pin(async move { p.base.set_system_profile(&profile_uuid).await })
        })
        .await
    }

    /// Set a TV's primary input — the HDMI port the controlling PC
    /// is plugged into. Drives `Tv1.Automated` recomputation.
    ///
    /// Routes through the per-object `Tv1.PrimaryInput` writable
    /// property.
    pub async fn set_tv_primary_input(&self, tv_id: &str, input: &str) -> Result<()> {
        let input = input.to_string();
        self.with_proxies(tv_id, "set primary input", move |p| {
            let input = input.clone();
            Box::pin(async move { p.base.set_primary_input(&input).await })
        })
        .await
    }
}

// =============================================================================
// Session Service Client (for tray/GUI to query user profiles)
// =============================================================================

/// D-Bus proxy for the tisvlay Session interface.
#[proxy(
    interface = "org.tisvlay.Session1",
    default_service = "org.tisvlay.Session",
    default_path = "/org/tisvlay/Session"
)]
trait TisvlaySession {
    /// Compile-time version of the running session-service binary.
    #[zbus(property(emits_changed_signal = "false"))]
    fn version(&self) -> zbus::Result<String>;

    /// Resolved on-disk path of the user's config file. Empty string
    /// when XDG resolution fails.
    #[zbus(property(emits_changed_signal = "false"))]
    fn config_path(&self) -> zbus::Result<String>;

    /// JSON-shape user-profile catalog. Returns `(name, json)` pairs
    /// where the JSON serializes a `UserProfileConfig`. Bound to the
    /// `ListUserProfilesJson` wire slot — the `ListUserProfiles` wire
    /// name carries the property-bag `a{sa{sv}}` shape.
    #[zbus(name = "ListUserProfilesJson")]
    fn list_user_profiles(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Get the user profile UUID assigned to a TV (string-formatted).
    fn get_tv_user_profile(&self, tv_id: &str) -> zbus::Result<String>;

    /// Assign a user profile to a TV by UUID (string-formatted).
    fn set_tv_user_profile(&self, tv_id: &str, profile_uuid: &str) -> zbus::Result<()>;

    /// JSON-shape effective profile. Bound to the
    /// `GetEffectiveUserProfileJson` wire slot — the
    /// `GetEffectiveUserProfile` wire name carries the property-bag
    /// `a{sv}` shape via the new CRUD surface.
    #[zbus(name = "GetEffectiveUserProfileJson")]
    fn get_effective_user_profile(&self, tv_id: &str) -> zbus::Result<String>;

    // ---- User-Profile property-bag CRUD surface --------------------------

    /// Catalog list: every user profile keyed by UUID string. Wire
    /// shape `a{sa{sv}}` (uuid → body). Includes the reserved-default
    /// UUID body (admin baseline when no user body shadows it).
    #[zbus(name = "ListUserProfiles")]
    fn list_user_profile_bodies(&self) -> zbus::Result<HashMap<String, UserProfileBody>>;

    /// Fetch one body by UUID string. Malformed or absent UUIDs
    /// return `org.tisvlay.Session1.Error.ProfileNotFound`.
    fn get_user_profile(&self, uuid: &str) -> zbus::Result<UserProfileBody>;

    /// Atomic full-body replace. Renames fold into this method —
    /// the new name lives on `body.name`.
    fn update_user_profile(&self, uuid: &str, body: UserProfileBody) -> zbus::Result<()>;

    /// Mint a new UUID under the given display name. Empty / reserved
    /// / duplicate names are rejected with
    /// `org.tisvlay.Session1.Error.ProfileNameInvalid`. Returns the
    /// new UUID as a string.
    fn create_user_profile(&self, name: &str) -> zbus::Result<String>;

    /// Clone an existing body under a new UUID + name. Returns the
    /// new UUID as a string.
    fn duplicate_user_profile(&self, source_uuid: &str, new_name: &str) -> zbus::Result<String>;

    /// Delete a profile. Rejects the reserved default UUID
    /// (`org.tisvlay.Session1.Error.ReservedProfile`) and rejects if
    /// at least one TV references the target via `[[tv]] profile.user`
    /// (`org.tisvlay.Session1.Error.ProfileReferenced`).
    fn delete_user_profile(&self, uuid: &str) -> zbus::Result<()>;

    /// Hard-coded baseline body — every user profile inherits from
    /// this when its fields are all `None`. Static.
    fn get_resolved_user_profile_defaults(&self) -> zbus::Result<UserProfileBody>;

    /// Per-TV resolved walk: returns the fully-resolved body for
    /// the named TV's assigned user profile (per-TV body → user
    /// default body → admin baseline → hardcoded defaults).
    fn get_effective_user_profile_body(&self, tv_id: &str) -> zbus::Result<UserProfileBody>;

    /// A user profile's body was mutated (any field including
    /// `name`). Clients with a cached body should re-fetch via
    /// `GetUserProfile(uuid)`.
    #[zbus(signal)]
    fn user_profile_changed(&self, uuid: String) -> zbus::Result<()>;

    /// A new user profile appeared in the catalog (Create or
    /// Duplicate).
    #[zbus(signal)]
    fn user_profile_added(&self, uuid: String) -> zbus::Result<()>;

    /// A user profile was removed from the catalog.
    #[zbus(signal)]
    fn user_profile_removed(&self, uuid: String) -> zbus::Result<()>;

    /// Reload user configuration from disk.
    fn reload_config(&self) -> zbus::Result<()>;
}

/// Client for communicating with the tisvlay session service via D-Bus.
///
/// This client provides a high-level interface for profile management
/// from the tray app or GUI.
#[derive(Clone, Debug)]
pub struct SessionClient {
    proxy: TisvlaySessionProxy<'static>,
}

impl SessionClient {
    /// Create a new session client connected to the session bus.
    ///
    /// There is intentionally no `new_system()` variant: the session service
    /// (`tisvlay-session`) always registers on the session bus regardless of
    /// whether the daemon uses the system or session bus. The `--user` flag
    /// affects `DaemonClient` only.
    pub async fn new() -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;

        Self::from_connection(connection).await
    }

    /// Create a client from an existing D-Bus connection. Mirror of
    /// [`DaemonClient::from_connection`] — used by tests pointing at
    /// a private `dbus-daemon` to bypass `zbus`'s per-process
    /// `OnceCell` cache.
    pub async fn from_connection(connection: zbus::Connection) -> Result<Self> {
        let proxy = TisvlaySessionProxy::new(&connection)
            .await
            .context("Failed to create session proxy")?;

        Ok(Self { proxy })
    }

    /// Check if the session service is available. Pings the
    /// JSON-shape catalog (no body deserialization) so an availability
    /// probe doesn't drag in `UserProfileBody` deserialization failures.
    pub async fn is_available(&self) -> bool {
        self.proxy.list_user_profiles().await.is_ok()
    }

    /// Compile-time version of the running session-service binary.
    /// Returns the `Session1.Version` const property — surfaces
    /// installed-version drift between the calling process and the
    /// session service after a partial upgrade.
    pub async fn version(&self) -> Result<String> {
        self.proxy
            .version()
            .await
            .context("Failed to read session-service version")
    }

    /// Resolved on-disk path of the user's config file. Returns the
    /// `Session1.ConfigPath` const property — empty string if XDG
    /// resolution fails on the session-service side.
    pub async fn config_path(&self) -> Result<String> {
        self.proxy
            .config_path()
            .await
            .context("Failed to read session-service config path")
    }

    /// List all available user profiles.
    ///
    /// Returns: Vec of (profile_name, profile_json) tuples.
    pub async fn list_user_profiles(&self) -> Result<Vec<(String, String)>> {
        self.proxy
            .list_user_profiles()
            .await
            .context("Failed to list user profiles from session service")
    }

    /// Get the user profile UUID assigned to a TV (string-formatted).
    pub async fn get_tv_user_profile(&self, tv_id: &str) -> Result<String> {
        self.proxy
            .get_tv_user_profile(tv_id)
            .await
            .context("Failed to get TV user profile from session service")
    }

    /// Assign a user profile to a TV by UUID (string-formatted). The
    /// session service rejects unknown UUIDs with `InvalidArgs`.
    pub async fn set_tv_user_profile(&self, tv_id: &str, profile_uuid: &str) -> Result<()> {
        self.proxy
            .set_tv_user_profile(tv_id, profile_uuid)
            .await
            .with_context(|| {
                format!(
                    "Failed to set user profile '{}' for TV '{}'",
                    profile_uuid, tv_id
                )
            })
    }

    /// Reload user configuration from disk.
    pub async fn reload_config(&self) -> Result<()> {
        self.proxy
            .reload_config()
            .await
            .context("Failed to reload session configuration")
    }

    // -------------------------------------------------------------------------
    // User-Profile property-bag CRUD
    //
    // Mirrors the eight `Session1` body-dict methods. Bodies round-trip
    // as `UserProfileBody`; UUID returns parse from the wire's `String`
    // representation.
    // -------------------------------------------------------------------------

    /// Catalog list: every user profile keyed by parsed UUID. The
    /// session emits wire UUIDs that are always well-formed, so a
    /// parse failure here means a buggy peer; the unparsable entry
    /// is skipped with a debug log.
    pub async fn list_user_profile_bodies(&self) -> Result<HashMap<Uuid, UserProfileBody>> {
        debug!("Calling ListUserProfiles on session service");
        let raw = self
            .proxy
            .list_user_profile_bodies()
            .await
            .context("Failed to list user profile bodies from session service")?;
        let mut out = HashMap::with_capacity(raw.len());
        for (uuid_str, body) in raw {
            match Uuid::parse_str(&uuid_str) {
                Ok(uuid) => {
                    out.insert(uuid, body);
                }
                Err(e) => {
                    warn!(uuid = %uuid_str, error = %e, "Skipping malformed UUID from ListUserProfiles");
                }
            }
        }
        Ok(out)
    }

    /// Fetch one body by UUID. Malformed or absent UUIDs surface as
    /// `org.tisvlay.Session1.Error.ProfileNotFound`.
    pub async fn get_user_profile(&self, uuid: Uuid) -> Result<UserProfileBody> {
        debug!(uuid = %uuid, "Calling GetUserProfile on session service");
        let uuid_str = uuid.to_string();
        self.proxy
            .get_user_profile(&uuid_str)
            .await
            .with_context(|| format!("Failed to get user profile '{}'", uuid_str))
    }

    /// Atomic full-body replace. Includes the new name on `body.name`;
    /// rename is just an update with a fresh `name`.
    pub async fn update_user_profile(&self, uuid: Uuid, body: &UserProfileBody) -> Result<()> {
        debug!(uuid = %uuid, name = %body.name, "Calling UpdateUserProfile on session service");
        let uuid_str = uuid.to_string();
        self.proxy
            .update_user_profile(&uuid_str, body.clone())
            .await
            .with_context(|| format!("Failed to update user profile '{}'", uuid_str))
    }

    /// Mint a new user profile under the given display name. Returns
    /// the new UUID. Empty / reserved / duplicate names are rejected
    /// with `org.tisvlay.Session1.Error.ProfileNameInvalid`.
    pub async fn create_user_profile(&self, name: &str) -> Result<Uuid> {
        debug!(name = %name, "Calling CreateUserProfile on session service");
        let raw = self
            .proxy
            .create_user_profile(name)
            .await
            .with_context(|| format!("Failed to create user profile '{}'", name))?;
        Uuid::parse_str(&raw).with_context(|| {
            format!(
                "Session service returned malformed UUID '{}' from CreateUserProfile",
                raw
            )
        })
    }

    /// Clone `source_uuid`'s body under a new UUID + name. Returns
    /// the new UUID.
    pub async fn duplicate_user_profile(&self, source_uuid: Uuid, new_name: &str) -> Result<Uuid> {
        debug!(source_uuid = %source_uuid, new_name = %new_name, "Calling DuplicateUserProfile on session service");
        let source_str = source_uuid.to_string();
        let raw = self
            .proxy
            .duplicate_user_profile(&source_str, new_name)
            .await
            .with_context(|| {
                format!(
                    "Failed to duplicate user profile '{}' as '{}'",
                    source_str, new_name
                )
            })?;
        Uuid::parse_str(&raw).with_context(|| {
            format!(
                "Session service returned malformed UUID '{}' from DuplicateUserProfile",
                raw
            )
        })
    }

    /// Delete a profile. Rejects the reserved default UUID with
    /// `org.tisvlay.Session1.Error.ReservedProfile`; rejects when at
    /// least one TV references the target via `[[tv]] profile.user`
    /// with `org.tisvlay.Session1.Error.ProfileReferenced`.
    pub async fn delete_user_profile(&self, uuid: Uuid) -> Result<()> {
        debug!(uuid = %uuid, "Calling DeleteUserProfile on session service");
        let uuid_str = uuid.to_string();
        self.proxy
            .delete_user_profile(&uuid_str)
            .await
            .with_context(|| format!("Failed to delete user profile '{}'", uuid_str))
    }

    /// Hard-coded baseline body — the inheritance ancestor every
    /// user profile falls back to when its own fields are all `None`.
    /// Static; the session exposes it so clients don't have to
    /// duplicate the table.
    pub async fn get_resolved_user_profile_defaults(&self) -> Result<UserProfileBody> {
        debug!("Calling GetResolvedUserProfileDefaults on session service");
        self.proxy
            .get_resolved_user_profile_defaults()
            .await
            .context("Failed to get resolved user-profile defaults")
    }

    /// Per-TV resolved walk: the fully-resolved body the session
    /// will dispatch against (per-TV body → user default body →
    /// admin baseline → hardcoded defaults).
    pub async fn get_effective_user_profile_body(&self, tv_id: &str) -> Result<UserProfileBody> {
        debug!(tv_id = %tv_id, "Calling GetEffectiveUserProfile on session service");
        self.proxy
            .get_effective_user_profile_body(tv_id)
            .await
            .with_context(|| format!("Failed to get effective user profile for TV '{}'", tv_id))
    }

    /// Subscribe to `Session1.UserProfileChanged` signals.
    ///
    /// Emits the UUID (parsed) every time a user-profile body is
    /// mutated server-side. Set up the stream **before** the mutation
    /// you want to observe — signals emitted prior to subscription
    /// are dropped.
    pub async fn user_profile_changed_stream(&self) -> Result<impl Stream<Item = Uuid> + use<>> {
        let raw = self
            .proxy
            .receive_user_profile_changed()
            .await
            .context("failed to subscribe to UserProfileChanged signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Uuid::parse_str(&args.uuid).ok()
        }))
    }

    /// Subscribe to `Session1.UserProfileAdded` signals.
    pub async fn user_profile_added_stream(&self) -> Result<impl Stream<Item = Uuid> + use<>> {
        let raw = self
            .proxy
            .receive_user_profile_added()
            .await
            .context("failed to subscribe to UserProfileAdded signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Uuid::parse_str(&args.uuid).ok()
        }))
    }

    /// Subscribe to `Session1.UserProfileRemoved` signals.
    pub async fn user_profile_removed_stream(&self) -> Result<impl Stream<Item = Uuid> + use<>> {
        let raw = self
            .proxy
            .receive_user_profile_removed()
            .await
            .context("failed to subscribe to UserProfileRemoved signal")?;
        Ok(raw.filter_map(|sig| async move {
            let args = sig.args().ok()?;
            Uuid::parse_str(&args.uuid).ok()
        }))
    }

    /// Access the underlying D-Bus connection. Useful for tests and
    /// clients that need to set up custom proxies (e.g., raw
    /// `Proxy::call_method` to exercise the deserialize boundary).
    pub fn connection(&self) -> &zbus::Connection {
        self.proxy.inner().connection()
    }
}

// =============================================================================
// Tray Interface Proxy + Client
// =============================================================================
//
// Mirrored from `Manager1` / `Session1` shape: const properties for
// version + config_path, a typed dict-derive body property for the
// mutable settings, single atomic Update method, ReloadConfig for
// hand-edit reconciliation, SendTestNotification for GUI-driven
// stack verification. No custom signals — `NotificationSettings`
// auto-emits `org.freedesktop.DBus.Properties.PropertiesChanged` on
// every D-Bus-driven write, and `ReloadConfig` emits it manually for
// disk-edit propagation.

#[proxy(
    interface = "org.tisvlay.Tray1",
    default_service = "org.tisvlay.Tray",
    default_path = "/org/tisvlay/Tray"
)]
trait TisvlayTray {
    /// Compile-time version of the running tray binary.
    #[zbus(property(emits_changed_signal = "false"))]
    fn version(&self) -> zbus::Result<String>;

    /// Resolved path of the tray's config file (`~/.config/tisvlay/tray.toml`).
    #[zbus(property(emits_changed_signal = "false"))]
    fn config_path(&self) -> zbus::Result<String>;

    /// Current desktop-notification settings. Auto-emits
    /// `PropertiesChanged` on every write — the GUI's `StateWatcher`
    /// subscribes to that signal to keep its Slint global in sync.
    #[zbus(property)]
    fn notification_settings(&self) -> zbus::Result<NotificationSettingsBody>;

    /// Atomic multi-key update. Only `Some(_)` fields in the body are
    /// applied; `None` leaves the existing value intact. Unknown
    /// fields are rejected by the dict-derive's `deny_unknown_fields`.
    fn update_notification_settings(&self, updates: NotificationSettingsBody) -> zbus::Result<()>;

    /// Re-read `tray.toml` from disk and reconcile in-memory state.
    /// If any category changed, emits `PropertiesChanged` for
    /// `NotificationSettings` (zbus's auto-emit only fires on D-Bus
    /// property writes, not in-memory mutations).
    fn reload_config(&self) -> zbus::Result<()>;

    /// Emit a single `Normal`-urgency test notification. Bypasses the
    /// category gate so the GUI's "Send test notification" button can
    /// verify the freedesktop notification stack even when every
    /// category is off.
    fn send_test_notification(&self) -> zbus::Result<()>;
}

#[derive(Clone, Debug)]
pub struct TrayClient {
    proxy: TisvlayTrayProxy<'static>,
}

impl TrayClient {
    /// Create a new tray client connected to the user/session bus.
    /// The tray always registers on the session bus regardless of
    /// whether the daemon uses system or session — same convention
    /// as `SessionClient`.
    pub async fn new() -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;
        let proxy = TisvlayTrayProxy::new(&connection)
            .await
            .context("Failed to create tray proxy")?;
        Ok(Self { proxy })
    }

    /// Check if the tray service is registered on the user bus.
    /// Probes the `Version` const property (cheapest non-failing
    /// call) and treats any error as offline.
    pub async fn is_available(&self) -> bool {
        self.proxy.version().await.is_ok()
    }

    /// Compile-time version of the running tray binary.
    pub async fn version(&self) -> Result<String> {
        self.proxy
            .version()
            .await
            .context("Failed to read tray version")
    }

    /// Resolved on-disk path of the tray's config file.
    pub async fn config_path(&self) -> Result<String> {
        self.proxy
            .config_path()
            .await
            .context("Failed to read tray config path")
    }

    /// Fetch the current notification settings (full body, every field
    /// `Some` on the wire).
    pub async fn notification_settings(&self) -> Result<NotificationSettingsBody> {
        self.proxy
            .notification_settings()
            .await
            .context("Failed to read tray notification settings")
    }

    /// Atomic partial-update of notification settings. The GUI's
    /// per-switch dispatch wraps a single field into a body with only
    /// that field `Some`; multi-field callers fill in everything they
    /// want to change.
    pub async fn update_notification_settings(
        &self,
        updates: NotificationSettingsBody,
    ) -> Result<()> {
        self.proxy
            .update_notification_settings(updates)
            .await
            .context("Failed to update tray notification settings")
    }

    /// Fire a single test notification (bypasses the category gate).
    pub async fn send_test_notification(&self) -> Result<()> {
        self.proxy
            .send_test_notification()
            .await
            .context("Failed to send test notification")
    }

    /// Re-read `tray.toml` from disk; the tray emits `PropertiesChanged`
    /// for `NotificationSettings` if the on-disk body changed. Backs the
    /// GUI's Reload button so a hand-edit to `tray.toml` is picked up the
    /// same way the daemon and session services reload theirs.
    pub async fn reload_config(&self) -> Result<()> {
        self.proxy
            .reload_config()
            .await
            .context("Failed to reload tray configuration")
    }
}

// =============================================================================
// GUI Interface Proxy + Client
// =============================================================================
//
// Symmetric with Tray1. The GUI normally introspects itself directly
// via `CARGO_PKG_VERSION`, but owning a `Gui1` interface lets external
// tooling (busctl, monitoring scripts) walk the whole stack with a
// single probe pattern.

#[proxy(
    interface = "org.tisvlay.Gui1",
    default_service = "org.tisvlay.Gui",
    default_path = "/org/tisvlay/Gui"
)]
trait TisvlayGui {
    /// Compile-time version of the running GUI binary.
    #[zbus(property(emits_changed_signal = "false"))]
    fn version(&self) -> zbus::Result<String>;

    /// Resolved path of the GUI's config file (empty until window-
    /// state persistence or similar lands).
    #[zbus(property(emits_changed_signal = "false"))]
    fn config_path(&self) -> zbus::Result<String>;
}

#[derive(Clone, Debug)]
pub struct GuiClient {
    proxy: TisvlayGuiProxy<'static>,
}

impl GuiClient {
    /// Create a new GUI client connected to the user/session bus.
    pub async fn new() -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;
        let proxy = TisvlayGuiProxy::new(&connection)
            .await
            .context("Failed to create GUI proxy")?;
        Ok(Self { proxy })
    }

    /// Check if a GUI process is registered on the user bus.
    pub async fn is_available(&self) -> bool {
        self.proxy.version().await.is_ok()
    }

    /// Compile-time version of the running GUI binary.
    pub async fn version(&self) -> Result<String> {
        self.proxy
            .version()
            .await
            .context("Failed to read GUI version")
    }

    /// Resolved on-disk path of the GUI's config file.
    pub async fn config_path(&self) -> Result<String> {
        self.proxy
            .config_path()
            .await
            .context("Failed to read GUI config path")
    }
}

// =============================================================================
// State Watcher
// =============================================================================

/// A high-level event from the D-Bus service watcher.
///
/// Consumers receive these from [`StateWatcher::next()`] and decide
/// how to react (refresh UI, reconnect clients, etc.).
#[derive(Debug, Clone)]
pub enum StateEvent {
    /// One or more daemon state signals were received (debounced).
    ///
    /// The inner value is the first signal in the debounce window
    /// (useful for logging); additional signals were coalesced.
    DaemonStateChanged(TvStateChange),

    /// The daemon service appeared on D-Bus.
    DaemonAppeared,

    /// The daemon service disappeared from D-Bus.
    DaemonDisappeared,

    /// The session service appeared on D-Bus.
    SessionAppeared,

    /// The session service disappeared from D-Bus.
    SessionDisappeared,

    /// `Session1.TvUserProfileChanged(tv_id, profile_uuid)`: a TV's
    /// user-profile assignment changed via `SetTvUserProfile`.
    /// Distinct from [`Self::UserProfileChanged`], which fires on a
    /// body mutation of the named profile UUID.
    TvUserProfileChanged,

    /// `Manager1.SystemProfileChanged(uuid)`: a system profile body
    /// was mutated. Consumers with a cached body should re-fetch via
    /// `DaemonClient::get_system_profile(uuid)`.
    SystemProfileChanged(Uuid),

    /// `Manager1.SystemProfileAdded(uuid)`: a new system profile
    /// appeared in the catalog.
    SystemProfileAdded(Uuid),

    /// `Manager1.SystemProfileRemoved(uuid)`: a system profile was
    /// removed from the catalog.
    SystemProfileRemoved(Uuid),

    /// `Session1.UserProfileChanged(uuid)`: a user profile body was
    /// mutated. Distinct from [`Self::TvUserProfileChanged`], which
    /// fires on a per-TV assignment change.
    UserProfileChanged(Uuid),

    /// `Session1.UserProfileAdded(uuid)`: a new user profile appeared
    /// in the catalog.
    UserProfileAdded(Uuid),

    /// `Session1.UserProfileRemoved(uuid)`: a user profile was
    /// removed from the catalog.
    UserProfileRemoved(Uuid),

    /// The tray service appeared on D-Bus.
    TrayAppeared,

    /// The tray service disappeared from D-Bus.
    TrayDisappeared,

    /// `Tray1.NotificationSettings` property changed (via D-Bus
    /// `UpdateNotificationSettings` or `ReloadConfig`). Payload-less;
    /// subscribers re-fetch via `TrayClient::notification_settings`.
    TrayNotificationSettingsChanged,

    /// The fallback poll timer fired.
    FallbackPoll,
}

/// Configuration for [`StateWatcher`].
#[derive(Debug, Clone)]
pub struct StateWatcherConfig {
    /// Duration to debounce daemon state signals (default: 100ms).
    pub signal_debounce: std::time::Duration,
    /// Interval for the fallback poll timer (default: 30s).
    pub fallback_poll_interval: std::time::Duration,
}

impl Default for StateWatcherConfig {
    fn default() -> Self {
        Self {
            signal_debounce: std::time::Duration::from_millis(100),
            fallback_poll_interval: std::time::Duration::from_secs(30),
        }
    }
}

/// Watches D-Bus services and signals, emitting [`StateEvent`]s.
///
/// Encapsulates all the D-Bus subscription and lifecycle watching logic
/// common across consumers (tray, GUI, etc.). The consumer drives the
/// event loop by calling [`next()`](Self::next).
///
/// # Setup
///
/// On construction, the watcher subscribes to:
/// - Daemon `PropertiesChanged`, `ObjectManager`, and `Manager1` signals
/// - Daemon well-known name appear/disappear (`NameOwnerChanged`)
/// - Session service well-known name appear/disappear
/// - Session `TvUserProfileChanged` + `UserProfileChanged` signals
///
/// # Example
///
/// ```ignore
/// let daemon = DaemonClient::new_session().await?;
/// let mut watcher = StateWatcher::new(&daemon, Default::default()).await?;
///
/// loop {
///     let Some(event) = watcher.next().await else { break };
///     match event {
///         StateEvent::DaemonStateChanged(_) | StateEvent::FallbackPoll => {
///             // refresh UI state
///         }
///         StateEvent::DaemonDisappeared => {
///             // mark all TVs unavailable
///         }
///         // ...
///         _ => {}
///     }
/// }
/// ```
/// Catalog-level system-profile signal kinds emitted by `Manager1`.
///
/// Used internally by [`StateWatcher`] to demultiplex the daemon-side
/// signal stream — each variant maps 1:1 to the matching `StateEvent`.
#[derive(Debug, Clone, Copy)]
enum SystemProfileSignal {
    Changed,
    Added,
    Removed,
}

/// Session-bus signals the watcher consumes — body mutations carry a
/// profile UUID; assignment changes carry only the TV id (dropped here
/// since `StateEvent::TvUserProfileChanged` is a payload-less refresh
/// hint today).
///
/// Body-level signals (`UserProfileChanged`, `UserProfileAdded`,
/// `UserProfileRemoved`) carry the profile UUID parsed from the wire
/// argument. `TvAssignmentChanged` is the per-TV
/// `TvUserProfileChanged(tv_id, profile_uuid)` signal.
#[derive(Debug, Clone)]
enum SessionSignal {
    TvAssignmentChanged,
    Changed(Uuid),
    Added(Uuid),
    Removed(Uuid),
}

/// Type alias for the `Box<dyn Stream<Item = T> + Send>` shape used by
/// every signal stream the watcher owns. Factored out to satisfy
/// `clippy::type_complexity` and to keep the field declarations
/// readable.
type BoxedStream<T> = std::pin::Pin<Box<dyn Stream<Item = T> + Send>>;

pub struct StateWatcher {
    /// Stream of daemon PropertiesChanged / ObjectManager signals.
    signal_stream: Option<BoxedStream<TvStateChange>>,
    /// Stream of `Manager1.SystemProfile{Changed,Added,Removed}` signals
    /// — each carries the parsed UUID from the wire argument.
    system_profile_signal_stream: Option<BoxedStream<(SystemProfileSignal, Uuid)>>,
    /// Stream of daemon service appear/disappear events.
    daemon_watch: Option<BoxedStream<ServiceEvent>>,
    /// Stream of session service appear/disappear events.
    session_watch: Option<BoxedStream<ServiceEvent>>,
    /// Stream of session-side signals: per-TV
    /// `TvUserProfileChanged` (assignment) plus the body-level
    /// `UserProfile{Changed,Added,Removed}` triad. A single
    /// MessageStream filters all four wire signatures.
    session_signal_stream: Option<BoxedStream<SessionSignal>>,
    /// Stream of tray service appear/disappear events.
    tray_watch: Option<BoxedStream<ServiceEvent>>,
    /// Stream of `Tray1.NotificationSettings` `PropertiesChanged`
    /// events — filtered from the session bus by interface +
    /// changed-property-name. Payload-less downstream (subscribers
    /// re-fetch via `TrayClient::notification_settings`).
    tray_properties_stream: Option<BoxedStream<()>>,
    /// Configuration.
    config: StateWatcherConfig,
}

impl std::fmt::Debug for StateWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateWatcher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl StateWatcher {
    /// Create a new watcher for the given daemon client.
    ///
    /// Uses `daemon.connection()` to derive the correct bus for the daemon
    /// name watch. Session service watching always uses the session bus.
    pub async fn new(daemon: &DaemonClient, config: StateWatcherConfig) -> Result<Self> {
        let session_bus = zbus::Connection::session().await.ok();
        Self::from_connections(daemon.connection(), session_bus.as_ref(), config).await
    }

    /// Create a watcher from explicit daemon and session connections.
    ///
    /// `daemon_conn` is used for `PropertiesChanged` / `ObjectManager` /
    /// `Manager1` signal subscriptions and the daemon name watch.
    /// `session_conn` is used for session-service name watching and
    /// `TvUserProfileChanged` + `UserProfileChanged` signals; pass
    /// `None` to skip session watching.
    ///
    /// In tests, both arguments can point at the same private bus —
    /// the daemon and session-service registrations live on one bus
    /// and the watcher subscribes to both via the same connection.
    /// In production, [`StateWatcher::new`] supplies the system bus
    /// (or session bus in dev mode) for the daemon and the user
    /// session bus for the session service.
    pub async fn from_connections(
        daemon_conn: &zbus::Connection,
        session_conn: Option<&zbus::Connection>,
        config: StateWatcherConfig,
    ) -> Result<Self> {
        use crate::dbus_paths::{SESSION_INTERFACE, SESSION_PATH, SESSION_SERVICE};

        let conn = daemon_conn;

        // --- Daemon state signals ---

        add_signal_match(
            conn,
            &format!(
                "type='signal',sender='{}',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path_namespace='{}'",
                DAEMON_SERVICE, TV_BASE_PATH
            ),
        )
        .await?;

        add_signal_match(
            conn,
            &format!(
                "type='signal',sender='{}',interface='org.freedesktop.DBus.ObjectManager',path='{}'",
                DAEMON_SERVICE,
                crate::dbus_paths::OBJECT_MANAGER_PATH,
            ),
        )
        .await?;

        add_signal_match(
            conn,
            &format!(
                "type='signal',sender='{}',interface='org.tisvlay.Manager1',path='{}'",
                DAEMON_SERVICE, MANAGER_PATH,
            ),
        )
        .await?;

        let tv_prefix = format!("{}/", TV_BASE_PATH);
        let raw_stream = zbus::MessageStream::from(conn.clone());

        // The TV-state filter consumes message *clones* and returns
        // `None` for `Manager1.SystemProfile*` signals; the
        // body-mutation path below filters the same shared stream for
        // those, so splitting the dispatch by interface+member keeps
        // both branches typed cleanly without dropping any signal.
        let tv_prefix_for_state = tv_prefix.clone();
        let raw_for_state = raw_stream.clone();
        let signal_stream: BoxedStream<TvStateChange> =
            Box::pin(raw_for_state.filter_map(move |msg_result| {
                let tv_prefix = tv_prefix_for_state.clone();
                async move {
                    let msg = msg_result.ok()?;
                    let header = msg.header();
                    let iface = header.interface()?.to_string();
                    let member = header.member()?.to_string();
                    let path = header.path()?.to_string();

                    match (iface.as_str(), member.as_str()) {
                        ("org.freedesktop.DBus.Properties", "PropertiesChanged") => {
                            let tv_id = path.strip_prefix(&tv_prefix)?.to_string();
                            trace!(tv_id = %tv_id, "PropertiesChanged signal on TV");
                            Some(TvStateChange::PropertiesChanged { tv_id })
                        }
                        ("org.freedesktop.DBus.ObjectManager", "InterfacesAdded")
                        | ("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved") => {
                            trace!(member = %member, "ObjectManager signal");
                            Some(TvStateChange::TvListChanged)
                        }
                        ("org.tisvlay.Manager1", "TvAdded" | "TvRemoved") => {
                            trace!(member = %member, "Manager1 TV list signal");
                            Some(TvStateChange::TvListChanged)
                        }
                        _ => None,
                    }
                }
            }));

        // --- Manager1.SystemProfile{Changed,Added,Removed} signals ---
        //
        // The body-mutation signals share the bus's existing
        // `org.tisvlay.Manager1` `add_signal_match` rule installed
        // above, so no extra match rule is needed; we just filter the
        // same MessageStream for the three body-mutation members. The
        // wire argument is the profile UUID as a string; parse to
        // `Uuid` here so the StateEvent payload is typed.
        let system_profile_signal_stream: BoxedStream<(SystemProfileSignal, Uuid)> =
            Box::pin(raw_stream.filter_map(|msg_result| async move {
                let msg = msg_result.ok()?;
                let header = msg.header();
                let iface = header.interface()?.to_string();
                let member = header.member()?.to_string();
                if iface != "org.tisvlay.Manager1" {
                    return None;
                }
                let kind = match member.as_str() {
                    "SystemProfileChanged" => SystemProfileSignal::Changed,
                    "SystemProfileAdded" => SystemProfileSignal::Added,
                    "SystemProfileRemoved" => SystemProfileSignal::Removed,
                    _ => return None,
                };
                // The wire signature is `(s)` — UUID as string. zbus's
                // `Message::body()` deserializes to whatever shape the
                // caller asks for; `String` is the only argument.
                let uuid_str: String = msg.body().deserialize().ok()?;
                let uuid = Uuid::parse_str(&uuid_str).ok()?;
                trace!(uuid = %uuid, kind = ?kind, "Manager1 system-profile signal");
                Some((kind, uuid))
            }));

        // --- Daemon name watch ---
        let daemon_watch: Option<std::pin::Pin<Box<dyn Stream<Item = ServiceEvent> + Send>>> =
            match watch_service(conn, DAEMON_SERVICE).await {
                Ok(stream) => Some(Box::pin(stream)),
                Err(e) => {
                    warn!(error = %e, "Failed to watch daemon service name");
                    None
                }
            };

        // --- Session name watch ---
        let session_watch: Option<std::pin::Pin<Box<dyn Stream<Item = ServiceEvent> + Send>>> =
            match session_conn {
                Some(sconn) => match watch_service(sconn, SESSION_SERVICE).await {
                    Ok(stream) => Some(Box::pin(stream)),
                    Err(e) => {
                        warn!(error = %e, "Failed to watch session service name");
                        None
                    }
                },
                None => None,
            };

        // --- Tray name watch (parallel to session) ---
        let tray_watch: Option<std::pin::Pin<Box<dyn Stream<Item = ServiceEvent> + Send>>> =
            match session_conn {
                Some(sconn) => match watch_service(sconn, crate::dbus_paths::TRAY_SERVICE).await {
                    Ok(stream) => Some(Box::pin(stream)),
                    Err(e) => {
                        warn!(error = %e, "Failed to watch tray service name");
                        None
                    }
                },
                None => None,
            };

        // --- Tray1.PropertiesChanged subscription ---
        //
        // Listens to the standard `org.freedesktop.DBus.Properties`
        // signal scoped to the tray's object path; filters to events
        // that touch the `NotificationSettings` property (either in
        // the changed-dict or the invalidated-array). Auto-emitted by
        // zbus on `Tray1.UpdateNotificationSettings`; manually emitted
        // by `Tray1.ReloadConfig` after a disk-edit diff.
        //
        // Per project convention, the downstream `StateEvent` is
        // payload-less — subscribers re-fetch via `TrayClient::
        // notification_settings` to get the canonical new body.
        let tray_properties_stream: Option<BoxedStream<()>> = match session_conn {
            Some(sconn) => {
                let rule = format!(
                    "type='signal',sender='{}',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='{}'",
                    crate::dbus_paths::TRAY_SERVICE,
                    crate::dbus_paths::TRAY_PATH,
                );
                match subscribe_signals(sconn, &rule).await {
                    Ok(raw) => {
                        let filtered = raw.filter_map(|msg_result| async move {
                            let msg = msg_result.ok()?;
                            // PropertiesChanged signature: (s, a{sv}, as)
                            // — (interface_name, changed_properties,
                            // invalidated_properties). We only care
                            // about the interface filter + whether
                            // `NotificationSettings` is in the changed
                            // dict or the invalidated array.
                            let (iface, changed, invalidated): (
                                String,
                                HashMap<String, OwnedValue>,
                                Vec<String>,
                            ) = msg.body().deserialize().ok()?;
                            if iface != crate::dbus_paths::TRAY_INTERFACE {
                                return None;
                            }
                            if changed.contains_key("NotificationSettings")
                                || invalidated.iter().any(|p| p == "NotificationSettings")
                            {
                                trace!("Tray1 NotificationSettings PropertiesChanged");
                                Some(())
                            } else {
                                None
                            }
                        });
                        debug!("Subscribed to Tray1 PropertiesChanged");
                        Some(Box::pin(filtered))
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to subscribe to Tray1 PropertiesChanged");
                        None
                    }
                }
            }
            None => None,
        };

        // --- Session1 signals: assignment + body triad ---
        //
        // `TvUserProfileChanged` carries `(tv_id, profile_uuid)` — the
        // tv_id is dropped here since the StateEvent variant is a
        // payload-less refresh hint. The body triad
        // (`UserProfileChanged`, `UserProfileAdded`, `UserProfileRemoved`)
        // carries the profile UUID as a string argument; we parse it
        // here so the StateEvent payload is typed. All four signals
        // share a single match rule by interface so one
        // `add_signal_match` covers the stream.
        let session_signal_stream: Option<
            std::pin::Pin<Box<dyn Stream<Item = SessionSignal> + Send>>,
        > = match session_conn {
            Some(sconn) => {
                let rule = format!(
                    "type='signal',sender='{}',interface='{}',path='{}'",
                    SESSION_SERVICE, SESSION_INTERFACE, SESSION_PATH,
                );
                match subscribe_signals(sconn, &rule).await {
                    Ok(raw) => {
                        let filtered = raw.filter_map(|msg_result| async move {
                            let msg = msg_result.ok()?;
                            let header = msg.header();
                            let member = header.member()?.to_string();
                            match member.as_str() {
                                "TvUserProfileChanged" => Some(SessionSignal::TvAssignmentChanged),
                                "UserProfileChanged" | "UserProfileAdded"
                                | "UserProfileRemoved" => {
                                    let uuid_str: String = msg.body().deserialize().ok()?;
                                    let uuid = Uuid::parse_str(&uuid_str).ok()?;
                                    Some(match member.as_str() {
                                        "UserProfileChanged" => SessionSignal::Changed(uuid),
                                        "UserProfileAdded" => SessionSignal::Added(uuid),
                                        "UserProfileRemoved" => SessionSignal::Removed(uuid),
                                        _ => unreachable!(),
                                    })
                                }
                                _ => None,
                            }
                        });
                        debug!("Subscribed to session signals");
                        Some(Box::pin(filtered))
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to subscribe to session signals");
                        None
                    }
                }
            }
            None => None,
        };

        info!("State watcher initialized");

        Ok(Self {
            signal_stream: Some(signal_stream),
            system_profile_signal_stream: Some(system_profile_signal_stream),
            daemon_watch,
            session_watch,
            session_signal_stream,
            tray_watch,
            tray_properties_stream,
            config,
        })
    }

    /// Receive the next state event.
    ///
    /// Daemon state signals are debounced: when the first signal arrives,
    /// additional signals within [`StateWatcherConfig::signal_debounce`] are
    /// drained before a single [`StateEvent::DaemonStateChanged`] is returned.
    ///
    /// Returns `None` if all underlying streams have closed (unlikely in
    /// practice since the fallback poll timer keeps the watcher alive).
    pub async fn next(&mut self) -> Option<StateEvent> {
        tokio::select! {
            // Branch 1: daemon state signal (debounced)
            Some(change) = async {
                match self.signal_stream.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                debug!(change = ?change, "State signal received, debouncing");
                let deadline = tokio::time::sleep(self.config.signal_debounce);
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        biased;
                        Some(_) = async {
                            match self.signal_stream.as_mut() {
                                Some(s) => s.next().await,
                                None => std::future::pending().await,
                            }
                        } => continue,
                        _ = &mut deadline => break,
                    }
                }
                Some(StateEvent::DaemonStateChanged(change))
            }

            // Branch 2: daemon appeared/disappeared
            Some(event) = async {
                match self.daemon_watch.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    ServiceEvent::Appeared => {
                        info!("Daemon appeared on D-Bus");
                        Some(StateEvent::DaemonAppeared)
                    }
                    ServiceEvent::Disappeared => {
                        info!("Daemon left D-Bus");
                        Some(StateEvent::DaemonDisappeared)
                    }
                }
            }

            // Branch 3: session service appeared/disappeared
            Some(event) = async {
                match self.session_watch.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    ServiceEvent::Appeared => {
                        info!("Session service appeared on D-Bus");
                        Some(StateEvent::SessionAppeared)
                    }
                    ServiceEvent::Disappeared => {
                        debug!("Session service left D-Bus");
                        Some(StateEvent::SessionDisappeared)
                    }
                }
            }

            // Branch 4: session-bus signals (assignment + body triad)
            Some(sig) = async {
                match self.session_signal_stream.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match sig {
                    SessionSignal::TvAssignmentChanged => {
                        debug!("Session TvUserProfileChanged signal received");
                        Some(StateEvent::TvUserProfileChanged)
                    }
                    SessionSignal::Changed(uuid) => {
                        debug!(uuid = %uuid, "Session UserProfileChanged signal received");
                        Some(StateEvent::UserProfileChanged(uuid))
                    }
                    SessionSignal::Added(uuid) => {
                        debug!(uuid = %uuid, "Session UserProfileAdded signal received");
                        Some(StateEvent::UserProfileAdded(uuid))
                    }
                    SessionSignal::Removed(uuid) => {
                        debug!(uuid = %uuid, "Session UserProfileRemoved signal received");
                        Some(StateEvent::UserProfileRemoved(uuid))
                    }
                }
            }

            // Branch 4b: Manager1 system-profile body-mutation signals
            Some((kind, uuid)) = async {
                match self.system_profile_signal_stream.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                Some(match kind {
                    SystemProfileSignal::Changed => {
                        debug!(uuid = %uuid, "Manager1 SystemProfileChanged signal received");
                        StateEvent::SystemProfileChanged(uuid)
                    }
                    SystemProfileSignal::Added => {
                        debug!(uuid = %uuid, "Manager1 SystemProfileAdded signal received");
                        StateEvent::SystemProfileAdded(uuid)
                    }
                    SystemProfileSignal::Removed => {
                        debug!(uuid = %uuid, "Manager1 SystemProfileRemoved signal received");
                        StateEvent::SystemProfileRemoved(uuid)
                    }
                })
            }

            // Branch 5: tray service appeared/disappeared
            Some(event) = async {
                match self.tray_watch.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    ServiceEvent::Appeared => {
                        info!("Tray service appeared on D-Bus");
                        Some(StateEvent::TrayAppeared)
                    }
                    ServiceEvent::Disappeared => {
                        debug!("Tray service left D-Bus");
                        Some(StateEvent::TrayDisappeared)
                    }
                }
            }

            // Branch 5b: Tray1 NotificationSettings PropertiesChanged
            Some(()) = async {
                match self.tray_properties_stream.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                debug!("Tray1 NotificationSettings PropertiesChanged received");
                Some(StateEvent::TrayNotificationSettingsChanged)
            }

            // Branch 6: fallback poll
            _ = tokio::time::sleep(self.config.fallback_poll_interval) => {
                debug!("Fallback poll timer fired");
                Some(StateEvent::FallbackPoll)
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a stream from owned tuples for unit-testing the drain helper.
    fn stream_from(
        events: Vec<(&str, &str, &str)>,
    ) -> impl futures_util::Stream<Item = (String, String, String)> {
        futures_util::stream::iter(
            events
                .into_iter()
                .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())),
        )
    }

    // ---------------------------------------------------------------------
    // drain_pair_to_terminal: success returns Complete with the tv_id +
    // message echoed from the terminal phase.
    // ---------------------------------------------------------------------
    #[tokio::test]
    async fn drain_pair_to_terminal_returns_complete_on_success() {
        let stream = stream_from(vec![
            ("tv-1", "awaiting_tv", "Accept on TV"),
            ("tv-1", "complete", "Pairing complete"),
        ]);
        let mut phases = Vec::new();
        let outcome = DaemonClient::drain_pair_to_terminal(stream, "tv-1", |p, m| {
            phases.push((p.to_string(), m.to_string()));
        })
        .await
        .unwrap();
        assert_eq!(
            outcome,
            PairTerminal::Complete {
                tv_id: "tv-1".to_string(),
                message: "Pairing complete".to_string(),
            }
        );
        assert_eq!(
            phases,
            vec![("awaiting_tv".to_string(), "Accept on TV".to_string())]
        );
    }

    // ---------------------------------------------------------------------
    // drain_pair_to_terminal: failure returns Failed with the reason from
    // the terminal phase's message field.
    // ---------------------------------------------------------------------
    #[tokio::test]
    async fn drain_pair_to_terminal_returns_failed_on_rejection() {
        let stream = stream_from(vec![
            ("tv-1", "awaiting_tv", "Accept on TV"),
            ("tv-1", "failed", "Pairing rejected by TV"),
        ]);
        let outcome = DaemonClient::drain_pair_to_terminal(stream, "tv-1", |_, _| {})
            .await
            .unwrap();
        assert_eq!(
            outcome,
            PairTerminal::Failed {
                tv_id: "tv-1".to_string(),
                message: "Pairing rejected by TV".to_string(),
            }
        );
    }

    // ---------------------------------------------------------------------
    // drain_pair_to_terminal: events for a different tv_id are silently
    // dropped — neither dispatched to `on_progress` nor used to resolve.
    // This is the property that lets concurrent pairings from multiple
    // clients coexist without bleeding into each other's UIs.
    // ---------------------------------------------------------------------
    #[tokio::test]
    async fn drain_pair_to_terminal_ignores_foreign_tv_ids() {
        let stream = stream_from(vec![
            // Concurrent foreign pair — drainer ignores all of these.
            ("tv-other", "awaiting_tv", "Other client's TV"),
            ("tv-other", "complete", "Other pair complete"),
            // Our pair starts after.
            ("tv-1", "awaiting_tv", "Accept on TV"),
            ("tv-1", "complete", "Pairing complete"),
        ]);
        let mut phases = Vec::new();
        let outcome = DaemonClient::drain_pair_to_terminal(stream, "tv-1", |p, m| {
            phases.push((p.to_string(), m.to_string()));
        })
        .await
        .unwrap();
        assert_eq!(
            outcome,
            PairTerminal::Complete {
                tv_id: "tv-1".to_string(),
                message: "Pairing complete".to_string(),
            }
        );
        assert_eq!(
            phases,
            vec![("awaiting_tv".to_string(), "Accept on TV".to_string())],
            "on_progress must not receive foreign tv_id events"
        );
    }

    // ---------------------------------------------------------------------
    // drain_pair_to_terminal: on_progress is called for every non-terminal
    // matching event in order; awaiting_input is a non-terminal phase that
    // exercises the multi-event path.
    // ---------------------------------------------------------------------
    #[tokio::test]
    async fn drain_pair_to_terminal_dispatches_all_non_terminal_phases() {
        let stream = stream_from(vec![
            ("tv-1", "awaiting_tv", "Step 1"),
            ("tv-1", "awaiting_input", "Step 2"),
            ("tv-1", "complete", "Pairing complete"),
        ]);
        let mut phases = Vec::new();
        let _ = DaemonClient::drain_pair_to_terminal(stream, "tv-1", |p, m| {
            phases.push((p.to_string(), m.to_string()));
        })
        .await
        .unwrap();
        assert_eq!(
            phases,
            vec![
                ("awaiting_tv".to_string(), "Step 1".to_string()),
                ("awaiting_input".to_string(), "Step 2".to_string()),
            ]
        );
    }

    // ---------------------------------------------------------------------
    // drain_pair_to_terminal: stream ending without a terminal phase
    // returns an error rather than hanging. This is the daemon-disconnect
    // failure mode — clients need to surface it as a generic failure.
    // ---------------------------------------------------------------------
    #[tokio::test]
    async fn drain_pair_to_terminal_errors_on_stream_end_before_terminal() {
        let stream = stream_from(vec![("tv-1", "awaiting_tv", "Accept on TV")]);
        let err = DaemonClient::drain_pair_to_terminal(stream, "tv-1", |_, _| {})
            .await
            .unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("ended before") && chain.contains("tv-1"),
            "error should explain the stream-end mode; got: {chain}"
        );
    }
}
