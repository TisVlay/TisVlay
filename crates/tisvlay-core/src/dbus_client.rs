//! D-Bus client for communicating with tisvlayd system daemon.
//!
//! This module provides a client for the `org.tisvlay.Daemon1` D-Bus interface,
//! allowing CLI and GUI applications to control TVs through the system daemon.
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
//!     for (id, name, ip, connected) in &tvs {
//!         println!("{}: {} ({}) - {}", id, name, ip,
//!             if *connected { "connected" } else { "disconnected" });
//!     }
//!
//!     // Turn off screen on first TV
//!     if let Some((id, _, _, _)) = tvs.first() {
//!         client.screen_off(id).await?;
//!     }
//!
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result};
use tracing::debug;
use zbus::proxy;

/// D-Bus service name for tisvlayd.
pub const DAEMON_SERVICE: &str = "org.tisvlay.Daemon";

/// D-Bus object path for tisvlayd.
pub const DAEMON_PATH: &str = "/org/tisvlay/Daemon";

/// D-Bus proxy for the tisvlay daemon.
///
/// This trait defines all methods available on the `org.tisvlay.Daemon1` interface.
#[proxy(
    interface = "org.tisvlay.Daemon1",
    default_service = "org.tisvlay.Daemon",
    default_path = "/org/tisvlay/Daemon"
)]
trait TisvlayDaemon {
    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// List configured TVs.
    /// Returns: [(id, name, ip, connected)]
    fn list_tvs(&self) -> zbus::Result<Vec<(String, String, String, bool)>>;

    /// Discover LG TVs on the network using SSDP.
    /// Returns: [(id, name, ip, mac)]
    fn discover_tvs(&self, timeout_ms: u32) -> zbus::Result<Vec<(String, String, String, String)>>;

    /// Pair a new TV.
    /// Returns: TV ID
    fn pair_tv(&self, ip: &str, name: &str, mac: &str) -> zbus::Result<String>;

    /// Remove a TV from configuration.
    fn remove_tv(&self, tv_id: &str) -> zbus::Result<()>;

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    fn power_on(&self, tv_id: &str) -> zbus::Result<()>;

    /// Power off a TV.
    fn power_off(&self, tv_id: &str) -> zbus::Result<()>;

    /// Turn screen on (resume from blank).
    fn screen_on(&self, tv_id: &str) -> zbus::Result<()>;

    /// Turn screen off (OLED-safe blank).
    fn screen_off(&self, tv_id: &str) -> zbus::Result<()>;

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch to a specific input.
    fn switch_input(&self, tv_id: &str, input_id: &str) -> zbus::Result<()>;

    /// Get available inputs.
    /// Returns: [(id, label)]
    fn get_inputs(&self, tv_id: &str) -> zbus::Result<Vec<(String, String)>>;

    // -------------------------------------------------------------------------
    // Audio Control
    // -------------------------------------------------------------------------

    /// Set volume (0-100).
    fn set_volume(&self, tv_id: &str, volume: u32) -> zbus::Result<()>;

    /// Get current volume.
    fn get_volume(&self, tv_id: &str) -> zbus::Result<u32>;

    /// Set mute state.
    fn set_mute(&self, tv_id: &str, mute: bool) -> zbus::Result<()>;

    // -------------------------------------------------------------------------
    // Picture Control
    // -------------------------------------------------------------------------

    /// Set backlight (OLED pixel brightness, 0-100).
    fn set_backlight(&self, tv_id: &str, backlight: u32) -> zbus::Result<()>;

    /// Get current backlight.
    fn get_backlight(&self, tv_id: &str) -> zbus::Result<u32>;

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state as JSON.
    fn get_state(&self, tv_id: &str) -> zbus::Result<String>;

    // -------------------------------------------------------------------------
    // Configuration
    // -------------------------------------------------------------------------

    /// Reload configuration from disk.
    fn reload_config(&self) -> zbus::Result<()>;
}

/// Client for communicating with the tisvlay system daemon via D-Bus.
///
/// This client provides a high-level interface for all TV control operations.
/// It automatically handles D-Bus connection setup and error conversion.
#[derive(Clone)]
pub struct DaemonClient {
    proxy: TisvlayDaemonProxy<'static>,
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
        let proxy = TisvlayDaemonProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(DAEMON_PATH)?
            .build()
            .await
            .context("Failed to create D-Bus proxy for tisvlayd")?;

        Ok(Self { proxy })
    }

    /// Check if the daemon is available and responding.
    pub async fn is_available(&self) -> bool {
        self.proxy.list_tvs().await.is_ok()
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// List all configured TVs.
    ///
    /// Returns a vector of tuples: `(id, name, ip, connected)`.
    pub async fn list_tvs(&self) -> Result<Vec<(String, String, String, bool)>> {
        self.proxy
            .list_tvs()
            .await
            .context("Failed to list TVs from daemon")
    }

    /// Get list of configured TV IDs.
    pub async fn list_tv_ids(&self) -> Result<Vec<String>> {
        let tvs = self.list_tvs().await?;
        Ok(tvs.into_iter().map(|(id, _, _, _)| id).collect())
    }

    /// Discover LG TVs on the network using SSDP.
    ///
    /// Returns a vector of tuples: `(id, name, ip, mac)`.
    /// Note: MAC address may be empty as SSDP doesn't always provide it.
    pub async fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> Result<Vec<(String, String, String, String)>> {
        self.proxy
            .discover_tvs(timeout_ms)
            .await
            .context("Failed to discover TVs")
    }

    /// Pair a new TV.
    ///
    /// This connects to the TV and triggers a pairing prompt on the TV screen.
    /// The user must accept the prompt for pairing to succeed.
    ///
    /// Returns the TV ID assigned to the new TV.
    pub async fn pair_tv(&self, ip: &str, name: &str, mac: Option<&str>) -> Result<String> {
        debug!(ip = %ip, name = %name, "Pairing TV via daemon");
        self.proxy
            .pair_tv(ip, name, mac.unwrap_or(""))
            .await
            .with_context(|| format!("Failed to pair TV at {}", ip))
    }

    /// Remove a TV from configuration.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Removing TV via daemon");
        self.proxy
            .remove_tv(tv_id)
            .await
            .with_context(|| format!("Failed to remove TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    pub async fn power_on(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling PowerOn on daemon");
        self.proxy
            .power_on(tv_id)
            .await
            .with_context(|| format!("Failed to power on TV: {}", tv_id))
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling PowerOff on daemon");
        self.proxy
            .power_off(tv_id)
            .await
            .with_context(|| format!("Failed to power off TV: {}", tv_id))
    }

    /// Turn screen on (resume from blank).
    pub async fn screen_on(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling ScreenOn on daemon");
        self.proxy
            .screen_on(tv_id)
            .await
            .with_context(|| format!("Failed to turn screen on for TV: {}", tv_id))
    }

    /// Turn screen off (OLED-safe blank).
    pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling ScreenOff on daemon");
        self.proxy
            .screen_off(tv_id)
            .await
            .with_context(|| format!("Failed to turn screen off for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch to a specific input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, input_id = %input_id, "Calling SwitchInput on daemon");
        self.proxy
            .switch_input(tv_id, input_id)
            .await
            .with_context(|| format!("Failed to switch input for TV: {}", tv_id))
    }

    /// Get available inputs.
    ///
    /// Returns a vector of tuples: `(id, label)`.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>> {
        debug!(tv_id = %tv_id, "Calling GetInputs on daemon");
        self.proxy
            .get_inputs(tv_id)
            .await
            .with_context(|| format!("Failed to get inputs for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Audio Control
    // -------------------------------------------------------------------------

    /// Set volume (0-100).
    pub async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<()> {
        debug!(tv_id = %tv_id, volume = volume, "Calling SetVolume on daemon");
        self.proxy
            .set_volume(tv_id, volume)
            .await
            .with_context(|| format!("Failed to set volume for TV: {}", tv_id))
    }

    /// Get current volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u32> {
        debug!(tv_id = %tv_id, "Calling GetVolume on daemon");
        self.proxy
            .get_volume(tv_id)
            .await
            .with_context(|| format!("Failed to get volume for TV: {}", tv_id))
    }

    /// Set mute state.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<()> {
        debug!(tv_id = %tv_id, mute = mute, "Calling SetMute on daemon");
        self.proxy
            .set_mute(tv_id, mute)
            .await
            .with_context(|| format!("Failed to set mute for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // Picture Control
    // -------------------------------------------------------------------------

    /// Set backlight (OLED pixel brightness, 0-100).
    pub async fn set_backlight(&self, tv_id: &str, backlight: u32) -> Result<()> {
        debug!(tv_id = %tv_id, backlight = backlight, "Calling SetBacklight on daemon");
        self.proxy
            .set_backlight(tv_id, backlight)
            .await
            .with_context(|| format!("Failed to set backlight for TV: {}", tv_id))
    }

    /// Get current backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u32> {
        debug!(tv_id = %tv_id, "Calling GetBacklight on daemon");
        self.proxy
            .get_backlight(tv_id)
            .await
            .with_context(|| format!("Failed to get backlight for TV: {}", tv_id))
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state as a JSON string.
    ///
    /// The returned JSON can be parsed into a `TvState` struct.
    pub async fn get_state_json(&self, tv_id: &str) -> Result<String> {
        debug!(tv_id = %tv_id, "Calling GetState on daemon");
        self.proxy
            .get_state(tv_id)
            .await
            .with_context(|| format!("Failed to get state for TV: {}", tv_id))
    }

    /// Get TV state as a parsed struct.
    pub async fn get_state(&self, tv_id: &str) -> Result<crate::TvState> {
        let json = self.get_state_json(tv_id).await?;
        serde_json::from_str(&json).context("Failed to parse TV state JSON")
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
        self.proxy
            .reload_config()
            .await
            .context("Failed to reload daemon configuration")
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
