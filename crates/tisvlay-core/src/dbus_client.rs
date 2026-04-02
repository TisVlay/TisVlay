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
use tracing::{debug, trace, warn};
use zbus::fdo::ObjectManagerProxy;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::dbus_paths::{tv_path, DAEMON_SERVICE, MANAGER_PATH, OBJECT_MANAGER_PATH, TV_BASE_PATH};

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
    /// Returns: [(id, name, ip, mac, vendor)]
    fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> zbus::Result<Vec<(String, String, String, String, String)>>;

    /// Get supported vendors.
    /// Returns: [(vendor_id, vendor_name)]
    fn get_supported_vendors(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Add a TV.
    /// config_json: vendor-specific connection parameters as JSON
    /// managed: whether the TV participates in automation
    /// Returns: TV object path
    fn add_tv(
        &self,
        name: &str,
        vendor: &str,
        config_json: &str,
        managed: bool,
    ) -> zbus::Result<OwnedObjectPath>;

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

    /// TV vendor ID.
    #[zbus(property)]
    fn vendor(&self) -> zbus::Result<String>;

    /// TV address (transport-agnostic display address).
    #[zbus(property)]
    fn address(&self) -> zbus::Result<String>;

    /// Whether the TV is currently connected.
    /// Uncached — always fetches from the daemon since connection status
    /// changes without emitting PropertiesChanged signals.
    #[zbus(property(emits_changed_signal = "false"))]
    fn connected(&self) -> zbus::Result<bool>;

    /// Whether this TV participates in automated management.
    #[zbus(property(emits_changed_signal = "false"))]
    fn managed(&self) -> zbus::Result<bool>;

    /// Capabilities supported by this TV.
    #[zbus(property)]
    fn capabilities(&self) -> zbus::Result<Vec<String>>;

    /// Force reconnection.
    fn reconnect(&self) -> zbus::Result<()>;

    /// Get TV state as JSON.
    fn get_state(&self) -> zbus::Result<String>;
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
}

/// D-Bus proxy for TV screen control.
#[proxy(
    interface = "org.tisvlay.Tv1.Screen",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvScreen {
    /// Turn screen on.
    fn screen_on(&self) -> zbus::Result<()>;

    /// Turn screen off.
    fn screen_off(&self) -> zbus::Result<()>;

    /// Whether screen is on.
    #[zbus(property)]
    fn screen_on_prop(&self) -> zbus::Result<bool>;
}

/// D-Bus proxy for TV volume control.
#[proxy(
    interface = "org.tisvlay.Tv1.Volume",
    default_service = "org.tisvlay.Daemon"
)]
trait TisvlayTvVolume {
    /// Set volume level.
    fn set_volume(&self, level: u8) -> zbus::Result<()>;

    /// Get current volume.
    #[zbus(property)]
    fn volume(&self) -> zbus::Result<u8>;

    /// Set mute state.
    fn set_mute(&self, mute: bool) -> zbus::Result<()>;

    /// Whether muted.
    #[zbus(property)]
    fn muted(&self) -> zbus::Result<bool>;
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
    /// Set backlight level.
    fn set_backlight(&self, level: u8) -> zbus::Result<()>;

    /// Get current backlight level.
    #[zbus(property)]
    fn backlight(&self) -> zbus::Result<u8>;
}

// =============================================================================
// TV State Change (for signal subscription)
// =============================================================================

/// A state change event received from the daemon via D-Bus signals.
#[derive(Debug, Clone)]
pub enum TvStateChange {
    /// TV state changed (volume, input, power, etc.).
    StateChanged { tv_id: String, state_json: String },
    /// TV connection status changed.
    ConnectionChanged { tv_id: String, connected: bool },
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
) -> Result<impl futures_util::Stream<Item = ServiceEvent>> {
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

/// Information about a configured TV.
#[derive(Debug, Clone)]
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
    /// System profile name assigned to this TV.
    pub system_profile: String,
    /// D-Bus object path.
    pub path: String,
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
    async fn from_connection(connection: zbus::Connection) -> Result<Self> {
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

    /// Subscribe to TV state change signals from the daemon.
    ///
    /// Returns a stream of [`TvStateChange`] events. The stream captures:
    /// - `StateChanged` and `ConnectionChanged` signals on `org.tisvlay.Tv1`
    ///   for any TV object under `/org/tisvlay/Tv/`
    /// - `InterfacesAdded` and `InterfacesRemoved` on the ObjectManager at
    ///   `/org/tisvlay` (TV add/remove)
    ///
    /// Works with both session and system bus (depends on how the client was
    /// constructed).
    pub async fn subscribe_state_changes(
        &self,
    ) -> Result<impl Stream<Item = TvStateChange> + Send + '_> {
        let conn = &self.connection;

        // Add match rules for all signal sources we care about.
        add_signal_match(
            conn,
            &format!(
                "type='signal',sender='{}',interface='org.tisvlay.Tv1',path_namespace='{}'",
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

        let mapped = raw_stream.filter_map(move |msg_result| {
            let tv_prefix = tv_prefix.clone();
            async move {
                let msg = msg_result.ok()?;
                let header = msg.header();
                let iface = header.interface()?.to_string();
                let member = header.member()?.to_string();
                let path = header.path()?.to_string();

                match (iface.as_str(), member.as_str()) {
                    ("org.tisvlay.Tv1", "StateChanged") => {
                        let tv_id = path.strip_prefix(&tv_prefix)?.to_string();
                        let body = msg.body();
                        let state_json: String = body.deserialize().ok()?;
                        trace!(tv_id = %tv_id, "StateChanged signal");
                        Some(TvStateChange::StateChanged { tv_id, state_json })
                    }
                    ("org.tisvlay.Tv1", "ConnectionChanged") => {
                        let tv_id = path.strip_prefix(&tv_prefix)?.to_string();
                        let body = msg.body();
                        let connected: bool = body.deserialize().ok()?;
                        trace!(tv_id = %tv_id, connected = connected, "ConnectionChanged signal");
                        Some(TvStateChange::ConnectionChanged { tv_id, connected })
                    }
                    ("org.freedesktop.DBus.ObjectManager", "InterfacesAdded")
                    | ("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved") => {
                        trace!(member = %member, "ObjectManager signal");
                        Some(TvStateChange::TvListChanged)
                    }
                    ("org.tisvlay.Manager1", "TvAdded") | ("org.tisvlay.Manager1", "TvRemoved") => {
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
        });

        Ok(mapped)
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
            let system_profile = {
                let sp = get_str("SystemProfile");
                if sp.is_empty() {
                    "default".to_string()
                } else {
                    sp
                }
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
                system_profile,
                path: path.as_str().to_string(),
            });
        }

        Ok(tvs)
    }

    /// Discover TVs on the network.
    ///
    /// Returns a vector of tuples: `(id, name, address, hardware_id, vendor)`.
    pub async fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> Result<Vec<(String, String, String, String, String)>> {
        self.manager
            .discover_tvs(timeout_ms)
            .await
            .context("Failed to discover TVs")
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
        let path = self
            .manager
            .add_tv(name, vendor, config_json, managed)
            .await
            .with_context(|| format!("Failed to pair TV '{}'", name))?;

        // Extract TV ID from path
        let tv_id = path
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or(path.as_str())
            .to_string();

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

    /// Turn screen on (resume from blank).
    pub async fn screen_on(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "turn screen on", |p| Box::pin(p.screen.screen_on()))
            .await
    }

    /// Turn screen off (OLED-safe blank).
    pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
        self.with_proxies(tv_id, "turn screen off", |p| {
            Box::pin(p.screen.screen_off())
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
        self.with_proxies(tv_id, "set mute", |p| Box::pin(p.volume.set_mute(mute)))
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

    /// Get TV state as a JSON string.
    ///
    /// The returned JSON can be parsed into a `TvState` struct.
    pub async fn get_state_json(&self, tv_id: &str) -> Result<String> {
        self.with_proxies(tv_id, "get state", |p| Box::pin(p.base.get_state()))
            .await
    }

    /// Get TV state as a parsed struct.
    pub async fn get_state(&self, tv_id: &str) -> Result<crate::TvState> {
        let json = self.get_state_json(tv_id).await?;
        serde_json::from_str(&json).context("Failed to parse TV state JSON")
    }

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
    /// Supported keys: `hdmi_input`, `only_when_active`, `managed`, `name`,
    /// `profile.system`.
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
#[derive(Clone)]
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
