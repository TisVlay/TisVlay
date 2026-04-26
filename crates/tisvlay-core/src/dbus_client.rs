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
//!         println!("{}: {} ({}) - {}", tv.id, tv.name, tv.address,
//!             if tv.connected { "connected" } else { "disconnected" });
//!     }
//!
//!     // Turn off screen on first TV
//!     if let Some(tv) = tvs.first() {
//!         client.screen_off(&tv.id).await?;
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
use zbus::fdo::ObjectManagerProxy;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::dbus_paths::{DAEMON_SERVICE, MANAGER_PATH, OBJECT_MANAGER_PATH, TV_BASE_PATH, tv_path};

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
// Manager Interface Proxy
// =============================================================================

/// D-Bus proxy for the tisvlay Manager interface.
#[proxy(
    interface = "org.tisvlay.Manager1",
    default_service = "org.tisvlay.Daemon",
    default_path = "/org/tisvlay/Manager"
)]
trait TisvlayManager {
    /// Discover TVs on the network.
    ///
    /// Returns `(id, name, address, (manufacturer, model, serial), vendor)` tuples.
    /// The nested `(sss)` carries the vendor-reported hardware identity with
    /// empty strings for unknown fields.
    fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> zbus::Result<Vec<(String, String, String, (String, String, String), String)>>;

    /// Get supported vendors.
    /// Returns: [(vendor_id, vendor_name)]
    fn get_supported_vendors(&self) -> zbus::Result<Vec<(String, String)>>;

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

    /// Remove a TV.
    fn remove_tv(&self, tv_path: &OwnedObjectPath) -> zbus::Result<()>;

    /// Reload configuration.
    fn reload_config(&self) -> zbus::Result<()>;

    /// Get the admin baseline for user profiles as JSON.
    fn get_user_profile_default(&self) -> zbus::Result<String>;

    /// Get all user profiles as (name, json) pairs.
    fn list_user_profiles(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Get all system profiles as (name, json) pairs.
    fn list_system_profiles(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Set a system-level TV option.
    fn set_tv_option(&self, tv_id: &str, key: &str, value: &str) -> zbus::Result<()>;

    /// Pairing-flow phase transition. Phase strings: `awaiting_tv`,
    /// `awaiting_input`, `complete`, `failed`. See
    /// [`tisvlay_core::PairingState`] for the canonical enum.
    #[zbus(signal)]
    fn pairing_progress(&self, tv_id: String, phase: String, message: String) -> zbus::Result<()>;

    /// One TV was discovered during a `DiscoverTvs` scan. Tuple shape
    /// matches the synchronous RPC return:
    /// `(id, name, address, (manufacturer, model, serial), vendor)`.
    #[zbus(signal)]
    fn discovery_found(
        &self,
        id: String,
        name: String,
        address: String,
        hardware_id: (String, String, String),
        vendor: String,
    ) -> zbus::Result<()>;
}

// =============================================================================
// TV Base Interface Proxy
// =============================================================================

/// D-Bus proxy for the base TV interface.
#[proxy(interface = "org.tisvlay.Tv1", default_service = "org.tisvlay.Daemon")]
trait TisvlayTv {
    /// TV identifier.
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    /// TV display name.
    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;

    /// Set TV display name.
    #[zbus(property)]
    fn set_name(&self, name: &str) -> zbus::Result<()>;

    /// TV vendor ID.
    #[zbus(property)]
    fn vendor(&self) -> zbus::Result<String>;

    /// TV address (transport-agnostic display address).
    #[zbus(property)]
    fn address(&self) -> zbus::Result<String>;

    /// Whether the TV is currently connected.
    #[zbus(property)]
    fn connected(&self) -> zbus::Result<bool>;

    /// Whether this TV participates in automated management.
    #[zbus(property)]
    fn managed(&self) -> zbus::Result<bool>;

    /// Set whether this TV participates in automated management.
    #[zbus(property)]
    fn set_managed(&self, managed: bool) -> zbus::Result<()>;

    /// System profile name assigned to this TV.
    #[zbus(property)]
    fn system_profile(&self) -> zbus::Result<String>;

    /// Set the system profile for this TV.
    #[zbus(property)]
    fn set_system_profile(&self, profile: &str) -> zbus::Result<()>;

    /// Vendor-reported hardware identity: `(manufacturer, model, serial)`.
    /// Empty strings represent unknown fields.
    #[zbus(property)]
    fn hardware_id(&self) -> zbus::Result<(String, String, String)>;

    /// Capabilities supported by this TV.
    #[zbus(property)]
    fn capabilities(&self) -> zbus::Result<Vec<String>>;

    /// Force reconnection.
    fn reconnect(&self) -> zbus::Result<()>;
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

    /// Phase transitions during a power-on flow.
    ///
    /// Phase string is one of `wol_sent`, `connecting`, `connected`,
    /// `failed` — see [`crate::PowerCyclePhase`] for the typed shape.
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
    #[zbus(property)]
    fn set_is_screen_on(&self, on: bool) -> zbus::Result<()>;
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
    #[zbus(property)]
    fn set_volume(&self, level: u8) -> zbus::Result<()>;

    /// Whether audio is currently muted.
    #[zbus(property)]
    fn muted(&self) -> zbus::Result<bool>;

    /// Set mute state.
    #[zbus(property)]
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
    #[zbus(property)]
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
    /// Daemon configuration was reloaded -- profile lists and TV properties may differ.
    ConfigReloaded,
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
    /// TV identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Transport-agnostic display address (e.g. IP, serial port, CEC address).
    pub address: String,
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
    /// System profile name assigned to this TV.
    pub system_profile: String,
    /// Primary-input ID for this TV (the HDMI port the controlling PC
    /// is plugged into). Empty when unset; "_unknown_" when set to
    /// the wildcard (every input is treated as primary).
    pub primary_input: String,
    /// Vendor-reported hardware identity (manufacturer / model / serial).
    pub hardware_id: crate::HardwareId,
    /// Desktop-side identity from the daemon's in-process DRM-EDID
    /// probe. Empty strings until `tisvlayd` matches a connector for
    /// this TV (typically on the first `ConnectionChanged(true)`).
    pub desktop: crate::DesktopMapping,
    /// D-Bus object path.
    pub path: String,

    // State from sub-interface properties
    /// Power state (e.g., "active", "standby", "screen_off", "unknown").
    pub power_state: String,
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

            let id = get_str("Id");
            let name = get_str("Name");
            let address = get_str("Address");
            let vendor = get_str("Vendor");
            let connected = get_bool("Connected", false);
            let managed = get_bool("Managed", true);
            let automated = get_bool("Automated", true);
            let system_profile = {
                let sp = get_str("SystemProfile");
                if sp.is_empty() {
                    "default".to_string()
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

            if id.is_empty() {
                warn!(path = %path, "TV object returned by daemon has empty Id, skipping");
                continue;
            }

            tvs.push(TvInfo {
                id,
                name,
                address,
                vendor,
                connected,
                managed,
                automated,
                system_profile,
                primary_input,
                hardware_id,
                desktop,
                path: path.as_str().to_string(),
                power_state,
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
    /// Returns `(id, name, address, hardware_id, vendor)` tuples.
    pub async fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> Result<Vec<(String, String, String, crate::HardwareId, String)>> {
        let raw = self
            .manager
            .discover_tvs(timeout_ms)
            .await
            .context("Failed to discover TVs")?;
        Ok(raw
            .into_iter()
            .map(|(id, name, address, hw_tuple, vendor)| {
                (
                    id,
                    name,
                    address,
                    crate::HardwareId::from_dbus_tuple(hw_tuple),
                    vendor,
                )
            })
            .collect())
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
    pub async fn remove_tv(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Removing TV via daemon");
        let path = tv_path(tv_id);
        let obj_path = OwnedObjectPath::try_from(path).context("Invalid TV path")?;
        self.manager
            .remove_tv(&obj_path)
            .await
            .with_context(|| format!("Failed to remove TV: {}", tv_id))?;

        // Remove from cache
        {
            let mut cache = self.tv_proxies.write().await;
            cache.remove(tv_id);
        }

        Ok(())
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

    /// Subscribe to `Manager1.DiscoveryFound` signals.
    ///
    /// Returns a stream of `(id, name, address, hardware_id, vendor)`
    /// tuples — same shape as the `discover_tvs` RPC return value.
    /// In v1 the daemon emits these in a burst right before
    /// `DiscoverTvs` returns; clients should still set up the stream
    /// before calling `discover_tvs` so future per-backend streaming
    /// arrives intact.
    pub async fn discovery_found_stream(
        &self,
    ) -> Result<
        impl futures_util::Stream<Item = (String, String, String, crate::HardwareId, String)> + use<>,
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
            ))
        }))
    }

    /// Turn screen on (resume from blank).
    pub async fn screen_on(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "turn screen on", |p| {
            Box::pin(p.screen.set_is_screen_on(true))
        })
        .await
    }

    /// Turn screen off (OLED-safe blank).
    pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "turn screen off", |p| {
            Box::pin(p.screen.set_is_screen_on(false))
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

    /// Set the system profile for a TV.
    ///
    /// Convenience wrapper around `set_tv_option(tv_id, "profile.system", name)`.
    pub async fn set_tv_system_profile(&self, tv_id: &str, profile_name: &str) -> Result<()> {
        self.set_tv_option(tv_id, "profile.system", profile_name)
            .await
    }

    /// Set a system-level TV option.
    ///
    /// Supported keys: `primary_input`, `managed`, `name`, `profile.system`.
    pub async fn set_tv_option(&self, tv_id: &str, key: &str, value: &str) -> Result<()> {
        debug!(tv_id = %tv_id, key = %key, value = %value, "Calling SetTvOption on daemon");
        self.manager
            .set_tv_option(tv_id, key, value)
            .await
            .with_context(|| format!("Failed to set TV option: {} {}={}", tv_id, key, value))
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
    /// List all available user profiles.
    fn list_user_profiles(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Get the user profile name assigned to a TV.
    fn get_tv_user_profile(&self, tv_id: &str) -> zbus::Result<String>;

    /// Assign a user profile to a TV.
    fn set_tv_user_profile(&self, tv_id: &str, profile_name: &str) -> zbus::Result<()>;

    /// Get the fully resolved user profile for a TV as JSON.
    fn get_effective_user_profile(&self, tv_id: &str) -> zbus::Result<String>;

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

        let proxy = TisvlaySessionProxy::new(&connection)
            .await
            .context("Failed to create session proxy")?;

        Ok(Self { proxy })
    }

    /// Check if the session service is available.
    pub async fn is_available(&self) -> bool {
        self.proxy.list_user_profiles().await.is_ok()
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

    /// Get the user profile name assigned to a TV.
    pub async fn get_tv_user_profile(&self, tv_id: &str) -> Result<String> {
        self.proxy
            .get_tv_user_profile(tv_id)
            .await
            .context("Failed to get TV user profile from session service")
    }

    /// Assign a user profile to a TV.
    pub async fn set_tv_user_profile(&self, tv_id: &str, profile_name: &str) -> Result<()> {
        self.proxy
            .set_tv_user_profile(tv_id, profile_name)
            .await
            .with_context(|| {
                format!(
                    "Failed to set user profile '{}' for TV '{}'",
                    profile_name, tv_id
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

    /// A `UserProfileChanged` signal was received from the session service.
    SessionProfileChanged,

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
/// - Session `UserProfileChanged` signals
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
pub struct StateWatcher {
    /// Stream of daemon PropertiesChanged / ObjectManager / ConfigReloaded signals.
    signal_stream: Option<std::pin::Pin<Box<dyn Stream<Item = TvStateChange> + Send>>>,
    /// Stream of daemon service appear/disappear events.
    daemon_watch: Option<std::pin::Pin<Box<dyn Stream<Item = ServiceEvent> + Send>>>,
    /// Stream of session service appear/disappear events.
    session_watch: Option<std::pin::Pin<Box<dyn Stream<Item = ServiceEvent> + Send>>>,
    /// Stream of session UserProfileChanged signals (filtered).
    session_signal_stream: Option<std::pin::Pin<Box<dyn Stream<Item = ()> + Send>>>,
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
    /// `UserProfileChanged` signals; pass `None` to skip session watching.
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

        let signal_stream: std::pin::Pin<Box<dyn Stream<Item = TvStateChange> + Send>> =
            Box::pin(raw_stream.filter_map(move |msg_result| {
                let tv_prefix = tv_prefix.clone();
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
                        ("org.tisvlay.Manager1", "ConfigReloaded") => {
                            trace!("Manager1 ConfigReloaded signal");
                            Some(TvStateChange::ConfigReloaded)
                        }
                        _ => None,
                    }
                }
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

        // --- Session UserProfileChanged signals ---
        let session_signal_stream: Option<std::pin::Pin<Box<dyn Stream<Item = ()> + Send>>> =
            match session_conn {
                Some(sconn) => {
                    let rule = format!(
                        "type='signal',sender='{}',interface='{}',member='UserProfileChanged',path='{}'",
                        SESSION_SERVICE, SESSION_INTERFACE, SESSION_PATH,
                    );
                    match subscribe_signals(sconn, &rule).await {
                        Ok(raw) => {
                            let filtered = raw.filter_map(|msg_result| async move {
                                let msg = msg_result.ok()?;
                                let header = msg.header();
                                let member = header.member()?.to_string();
                                if member == "UserProfileChanged" {
                                    Some(())
                                } else {
                                    None
                                }
                            });
                            debug!("Subscribed to session UserProfileChanged signals");
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
            daemon_watch,
            session_watch,
            session_signal_stream,
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

            // Branch 4: session UserProfileChanged signal
            Some(()) = async {
                match self.session_signal_stream.as_mut() {
                    Some(s) => s.next().await,
                    None => std::future::pending().await,
                }
            } => {
                debug!("Session UserProfileChanged signal received");
                Some(StateEvent::SessionProfileChanged)
            }

            // Branch 5: fallback poll
            _ = tokio::time::sleep(self.config.fallback_poll_interval) => {
                debug!("Fallback poll timer fired");
                Some(StateEvent::FallbackPoll)
            }
        }
    }
}

// =============================================================================
// RefreshTrigger
// =============================================================================

/// Spawn-with-dedupe trigger for refresh-style work driven by
/// [`StateWatcher`] events.
///
/// The canonical pattern for consuming a `StateWatcher` is to dispatch
/// each `DaemonStateChanged` (and friends) to `RefreshTrigger::fire()`,
/// which spawns the refresh on a separate task. The watcher loop body
/// MUST NOT `await` the refresh inline — see "Why this matters" below.
///
/// At most one refresh is in flight. Fires that arrive while a refresh
/// is running set a `dirty` flag; when the in-flight refresh completes
/// the worker checks the flag and runs another refresh without
/// releasing the in-flight slot. This guarantees trailing-edge
/// correctness — a flood that ends exactly during a refresh still
/// captures the post-flood state.
///
/// # Example
///
/// ```ignore
/// let trigger = RefreshTrigger::new();
/// loop {
///     let Some(event) = watcher.next().await else { break };
///     match event {
///         StateEvent::DaemonStateChanged(_)
///         | StateEvent::DaemonAppeared
///         | StateEvent::SessionProfileChanged
///         | StateEvent::FallbackPoll => {
///             let daemon = daemon.clone();
///             let session = session.clone();
///             let handle = handle.clone();
///             trigger.fire(move || {
///                 let daemon = daemon.clone();
///                 let session = session.clone();
///                 let handle = handle.clone();
///                 async move { refresh_state(&daemon, &session, &handle).await; }
///             });
///         }
///         /* … pane-specific dispatch for the other events … */
///     }
/// }
/// ```
///
/// # Why this matters
///
/// Awaiting refresh work inline in the `watcher.next()` loop body
/// halts signal_stream draining → zbus's bounded internal channel
/// fills → its I/O task blocks pushing → the connection stops reading
/// from the socket → method-call replies queue behind unread signal
/// frames in the kernel recv buffer. Subsequent client commands hang
/// waiting for replies that can't be read.
///
/// Verified repro for the bug fixed by introducing this helper: a
/// fast volume-slider drag in the GUI flooded the daemon, the tray's
/// recv-q grew to 36 KB, and a subsequent ScrollBy command hung
/// indefinitely while `busctl` against the same daemon replied in
/// 4 ms — confirming the daemon was healthy and the tray's
/// connection was wedged on its own consumption pattern.
pub struct RefreshTrigger {
    in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for RefreshTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::sync::atomic::Ordering;
        f.debug_struct("RefreshTrigger")
            .field("in_flight", &self.in_flight.load(Ordering::Relaxed))
            .field("dirty", &self.dirty.load(Ordering::Relaxed))
            .finish()
    }
}

impl RefreshTrigger {
    pub fn new() -> Self {
        Self {
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark a refresh as needed; spawn a worker task if one isn't
    /// already running. The worker loops as long as `dirty` is set,
    /// guaranteeing a final refresh after any flood that ended during
    /// an earlier iteration.
    ///
    /// `refresh_fn` may be invoked multiple times across iterations,
    /// so it's `Fn` (not `FnOnce`) and must be `Sync` because the
    /// worker runs on a tokio task. Closures that capture `Arc<T>`
    /// and clone inside their body satisfy this; closures that
    /// consume captured values do not, which is the desired
    /// behavior — those would need re-creation per fire.
    pub fn fire<F, Fut>(&self, refresh_fn: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        use std::sync::atomic::Ordering;
        self.dirty.store(true, Ordering::Relaxed);
        if self.in_flight.swap(true, Ordering::Relaxed) {
            return;
        }
        let in_flight = self.in_flight.clone();
        let dirty = self.dirty.clone();
        tokio::spawn(async move {
            // Loop until no fire happened during the last refresh —
            // captures any flood that ended exactly between iterations.
            while dirty.swap(false, Ordering::Relaxed) {
                refresh_fn().await;
            }
            in_flight.store(false, Ordering::Relaxed);
        });
    }
}

impl Default for RefreshTrigger {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Coalescer
// =============================================================================

/// Per-key coalescing dispatch.
///
/// Rapid `fire(key, value)` calls are folded by the user-supplied
/// `merge` function before `apply(key, merged_value)` runs. Bounds
/// in-flight work to one worker task per key, regardless of input
/// rate.
///
/// Common merge choices:
/// - **Last-wins** for absolute values (slider position):
///   `|_, new| new`
/// - **Sum** for relative deltas (scroll-wheel ticks):
///   `|old, new| old + new` (where `V: Add`)
///
/// The worker for a given key is spawned lazily on the first
/// `fire(key, _)` and lives forever. Each burst draws one value via
/// `recv()`, drains anything else queued via `try_recv()` folding
/// through `merge`, then runs `apply(key, merged)`. New values
/// arriving while `apply` is running queue and form the next burst.
///
/// Use cases in this workspace:
/// - GUI slider drags: bound daemon writes to one in-flight call per
///   `(tv_id, property)` regardless of drag rate (last-wins).
/// - Tray scroll wheel: fold rapid scroll ticks into one daemon call
///   per `(tv_id, target)` burst (sum).
///
/// `Clone` is cheap (clones internal `Arc`s); pass clones into
/// closures that need to fire from multiple call sites.
pub struct Coalescer<K, V> {
    inner: std::sync::Arc<CoalescerInner<K, V>>,
}

type ApplyFn<K, V> = Box<
    dyn Fn(K, V) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

struct CoalescerInner<K, V> {
    senders: std::sync::Mutex<std::collections::HashMap<K, tokio::sync::mpsc::UnboundedSender<V>>>,
    merge: Box<dyn Fn(V, V) -> V + Send + Sync>,
    apply: ApplyFn<K, V>,
}

impl<K, V> Clone for Coalescer<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V> Coalescer<K, V>
where
    K: std::hash::Hash + Eq + Clone + Send + 'static,
    V: Send + 'static,
{
    /// Construct a coalescer with the given merge and apply functions.
    ///
    /// `merge(a, b) -> c` is called as `c = merge(latest, next)` to
    /// fold values queued during a single burst.
    ///
    /// `apply(key, merged)` runs the actual work. The returned future
    /// is `await`ed by the per-key worker; the next burst can't start
    /// for that key until it completes.
    pub fn new<Merge, Apply, Fut>(merge: Merge, apply: Apply) -> Self
    where
        Merge: Fn(V, V) -> V + Send + Sync + 'static,
        Apply: Fn(K, V) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        Self {
            inner: std::sync::Arc::new(CoalescerInner {
                senders: std::sync::Mutex::new(std::collections::HashMap::new()),
                merge: Box::new(merge),
                apply: Box::new(move |k, v| Box::pin(apply(k, v))),
            }),
        }
    }

    /// Push `value` for `key`. Spawns a worker for this key if none
    /// exists yet. Non-blocking: returns immediately after enqueueing.
    pub fn fire(&self, key: K, value: V) {
        let mut map = self
            .inner
            .senders
            .lock()
            .expect("Coalescer senders mutex poisoned");
        let tx = if let Some(tx) = map.get(&key) {
            tx.clone()
        } else {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<V>();
            let key_inner = key.clone();
            let inner = self.inner.clone();
            tokio::spawn(async move {
                while let Some(mut latest) = rx.recv().await {
                    while let Ok(v) = rx.try_recv() {
                        latest = (inner.merge)(latest, v);
                    }
                    (inner.apply)(key_inner.clone(), latest).await;
                }
            });
            map.insert(key.clone(), tx.clone());
            tx
        };
        drop(map);
        let _ = tx.send(value);
    }
}

impl<K, V> std::fmt::Debug for Coalescer<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coalescer")
            .field(
                "active_keys",
                &self.inner.senders.lock().map(|m| m.len()).unwrap_or(0),
            )
            .finish()
    }
}
