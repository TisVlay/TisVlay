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
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;
use zbus::proxy;
use zbus::zvariant::OwnedObjectPath;

/// D-Bus service name for tisvlayd.
pub const DAEMON_SERVICE: &str = "org.tisvlay.Daemon";

/// D-Bus object path for manager interface.
pub const MANAGER_PATH: &str = "/org/tisvlay/Manager";

/// Base path for TV objects.
pub const TV_BASE_PATH: &str = "/org/tisvlay/Tv";

/// Generate the D-Bus object path for a TV.
pub fn tv_path(tv_id: &str) -> String {
    let sanitized: String = tv_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}/{}", TV_BASE_PATH, sanitized)
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
    /// Returns: [(id, name, ip, mac, vendor)]
    fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> zbus::Result<Vec<(String, String, String, String, String)>>;

    /// Get list of TV object paths.
    fn get_tvs(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Get supported vendors.
    /// Returns: [(vendor_id, vendor_name)]
    fn get_supported_vendors(&self) -> zbus::Result<Vec<(String, String)>>;

    /// Add a TV.
    /// config_json: vendor-specific connection parameters as JSON
    /// Returns: TV object path
    fn add_tv(&self, name: &str, vendor: &str, config_json: &str) -> zbus::Result<OwnedObjectPath>;

    /// Remove a TV.
    fn remove_tv(&self, tv_path: &OwnedObjectPath) -> zbus::Result<()>;

    /// Reload configuration.
    fn reload_config(&self) -> zbus::Result<()>;
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
    #[zbus(property)]
    fn connected(&self) -> zbus::Result<bool>;

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
    tv_proxies: Arc<RwLock<HashMap<String, TvProxies>>>,
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

        Ok(Self {
            connection,
            manager,
            tv_proxies: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get or create proxies for a TV.
    async fn get_tv_proxies(&self, tv_id: &str) -> Result<()> {
        // Check if already cached
        {
            let cache = self.tv_proxies.read().await;
            if cache.contains_key(tv_id) {
                return Ok(());
            }
        }

        // Create new proxies
        let path = tv_path(tv_id);
        let obj_path = OwnedObjectPath::try_from(path).context("Invalid TV path")?;
        let proxies = TvProxies::new(self.connection.clone(), obj_path).await?;

        // Cache them
        {
            let mut cache = self.tv_proxies.write().await;
            cache.insert(tv_id.to_string(), proxies);
        }

        Ok(())
    }

    /// Check if the daemon is available and responding.
    pub async fn is_available(&self) -> bool {
        self.manager.get_tvs().await.is_ok()
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// List all configured TVs.
    pub async fn list_tvs(&self) -> Result<Vec<TvInfo>> {
        let paths = self
            .manager
            .get_tvs()
            .await
            .context("Failed to list TVs from daemon")?;

        let mut tvs = Vec::new();
        for path in paths {
            let path_str = path.as_str();
            // Extract TV ID from path (last component)
            let tv_id = path_str.rsplit('/').next().unwrap_or(path_str).to_string();

            self.get_tv_proxies(&tv_id).await?;

            let cache = self.tv_proxies.read().await;
            if let Some(proxies) = cache.get(&tv_id) {
                let info = TvInfo {
                    id: proxies.base.id().await.unwrap_or_default(),
                    name: proxies.base.name().await.unwrap_or_default(),
                    address: proxies.base.address().await.unwrap_or_default(),
                    vendor: proxies.base.vendor().await.unwrap_or_default(),
                    connected: proxies.base.connected().await.unwrap_or(false),
                    path: path_str.to_string(),
                };
                tvs.push(info);
            }
        }

        Ok(tvs)
    }

    /// Get list of configured TV IDs.
    pub async fn list_tv_ids(&self) -> Result<Vec<String>> {
        let tvs = self.list_tvs().await?;
        Ok(tvs.into_iter().map(|tv| tv.id).collect())
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
    pub async fn pair_tv(&self, name: &str, vendor: &str, config_json: &str) -> Result<String> {
        debug!(name = %name, vendor = %vendor, "Pairing TV via daemon");
        let path = self
            .manager
            .add_tv(name, vendor, config_json)
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
        debug!(tv_id = %tv_id, "Calling PowerOn on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .power
            .power_on()
            .await
            .with_context(|| format!("Failed to power on TV: {}", tv_id))
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling PowerOff on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .power
            .power_off()
            .await
            .with_context(|| format!("Failed to power off TV: {}", tv_id))
    }

    /// Turn screen on (resume from blank).
    pub async fn screen_on(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling ScreenOn on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .screen
            .screen_on()
            .await
            .with_context(|| format!("Failed to turn screen on for TV: {}", tv_id))
    }

    /// Turn screen off (OLED-safe blank).
    pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling ScreenOff on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .screen
            .screen_off()
            .await
            .with_context(|| format!("Failed to turn screen off for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch to a specific input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, input_id = %input_id, "Calling SwitchInput on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .input
            .switch_input(input_id)
            .await
            .with_context(|| format!("Failed to switch input for TV: {}", tv_id))
    }

    /// Get available inputs.
    ///
    /// Returns a vector of tuples: `(id, label)`.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>> {
        debug!(tv_id = %tv_id, "Calling GetInputs on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .input
            .available_inputs()
            .await
            .with_context(|| format!("Failed to get inputs for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Audio Control
    // -------------------------------------------------------------------------

    /// Set volume (0-100).
    pub async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<()> {
        debug!(tv_id = %tv_id, volume = volume, "Calling SetVolume on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .volume
            .set_volume(volume as u8)
            .await
            .with_context(|| format!("Failed to set volume for TV: {}", tv_id))
    }

    /// Get current volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u32> {
        debug!(tv_id = %tv_id, "Calling GetVolume on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        let vol = proxies
            .volume
            .volume()
            .await
            .with_context(|| format!("Failed to get volume for TV: {}", tv_id))?;
        Ok(vol as u32)
    }

    /// Set mute state.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<()> {
        debug!(tv_id = %tv_id, mute = mute, "Calling SetMute on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .volume
            .set_mute(mute)
            .await
            .with_context(|| format!("Failed to set mute for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Picture Control
    // -------------------------------------------------------------------------

    /// Set backlight (OLED pixel brightness, 0-100).
    pub async fn set_backlight(&self, tv_id: &str, backlight: u32) -> Result<()> {
        debug!(tv_id = %tv_id, backlight = backlight, "Calling SetBacklight on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .backlight
            .set_backlight(backlight as u8)
            .await
            .with_context(|| format!("Failed to set backlight for TV: {}", tv_id))
    }

    /// Get current backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u32> {
        debug!(tv_id = %tv_id, "Calling GetBacklight on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        let bl = proxies
            .backlight
            .backlight()
            .await
            .with_context(|| format!("Failed to get backlight for TV: {}", tv_id))?;
        Ok(bl as u32)
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state as a JSON string.
    ///
    /// The returned JSON can be parsed into a `TvState` struct.
    pub async fn get_state_json(&self, tv_id: &str) -> Result<String> {
        debug!(tv_id = %tv_id, "Calling GetState on daemon");
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .base
            .get_state()
            .await
            .with_context(|| format!("Failed to get state for TV: {}", tv_id))
    }

    /// Get TV state as a parsed struct.
    pub async fn get_state(&self, tv_id: &str) -> Result<crate::TvState> {
        let json = self.get_state_json(tv_id).await?;
        serde_json::from_str(&json).context("Failed to parse TV state JSON")
    }

    /// Get TV capabilities.
    pub async fn get_capabilities(&self, tv_id: &str) -> Result<Vec<String>> {
        self.get_tv_proxies(tv_id).await?;
        let cache = self.tv_proxies.read().await;
        let proxies = cache
            .get(tv_id)
            .ok_or_else(|| anyhow::anyhow!("TV not found: {}", tv_id))?;
        proxies
            .base
            .capabilities()
            .await
            .with_context(|| format!("Failed to get capabilities for TV: {}", tv_id))
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

    // -------------------------------------------------------------------------
    // Utility Methods
    // -------------------------------------------------------------------------

    /// Execute an action on all configured TVs.
    ///
    /// Returns the number of successful operations.
    pub async fn execute_on_all_tvs<F, Fut>(&self, action_name: &str, action: F) -> usize
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let tv_ids = match self.list_tv_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("Failed to list TVs: {}", e);
                return 0;
            }
        };

        if tv_ids.is_empty() {
            debug!("No TVs configured, skipping {}", action_name);
            return 0;
        }

        let mut success_count = 0;
        for tv_id in tv_ids {
            match action(tv_id.clone()).await {
                Ok(()) => {
                    debug!(tv_id = %tv_id, action = %action_name, "Action succeeded");
                    success_count += 1;
                }
                Err(e) => {
                    tracing::warn!(tv_id = %tv_id, action = %action_name, error = %e, "Action failed");
                }
            }
        }

        success_count
    }
}
