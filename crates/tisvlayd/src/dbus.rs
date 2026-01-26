//! D-Bus interface implementation.
//!
//! Implements the `org.tisvlay.Daemon1` interface for TV control.

use crate::tv_manager::TvManager;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use zbus::interface;

/// D-Bus service for TV control.
pub struct DaemonService {
    manager: Arc<RwLock<TvManager>>,
}

impl DaemonService {
    /// Create a new D-Bus service with a shared manager.
    pub fn new_with_shared(manager: Arc<RwLock<TvManager>>) -> Self {
        Self { manager }
    }

    /// Create a new D-Bus service.
    #[allow(dead_code)]
    pub fn new(manager: TvManager) -> Self {
        Self::new_with_shared(Arc::new(RwLock::new(manager)))
    }
}

/// D-Bus error type for tisvlay.
#[derive(Debug, Clone, zbus::DBusError)]
#[zbus(prefix = "org.tisvlay.Error")]
pub enum DaemonError {
    /// TV not found.
    TvNotFound(String),
    /// TV not connected.
    NotConnected(String),
    /// Connection failed.
    ConnectionFailed(String),
    /// Operation failed.
    OperationFailed(String),
}

impl From<String> for DaemonError {
    fn from(s: String) -> Self {
        if s.contains("not found") {
            DaemonError::TvNotFound(s)
        } else if s.contains("Not connected") {
            DaemonError::NotConnected(s)
        } else if s.contains("Connection failed") {
            DaemonError::ConnectionFailed(s)
        } else {
            DaemonError::OperationFailed(s)
        }
    }
}

#[interface(name = "org.tisvlay.Daemon1")]
impl DaemonService {
    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// List configured TVs.
    /// Returns: [(id, name, ip, connected)]
    async fn list_tvs(&self) -> Vec<(String, String, String, bool)> {
        debug!("D-Bus: ListTvs");
        let manager = self.manager.read().await;
        manager.list_tvs().await
    }

    /// Discover LG TVs on the network using SSDP.
    ///
    /// Sends an M-SEARCH request and waits for TV responses.
    ///
    /// Arguments:
    /// - `timeout_ms`: How long to wait for TV responses (recommended: 2000-5000)
    ///
    /// Returns: Array of (id, name, ip, mac) tuples.
    /// Note: MAC address will be empty as SSDP doesn't provide it.
    #[cfg(feature = "discovery")]
    async fn discover_tvs(
        &self,
        timeout_ms: u32,
    ) -> Result<Vec<(String, String, String, String)>, DaemonError> {
        info!(timeout_ms = timeout_ms, "D-Bus: DiscoverTvs");

        let tvs = tisvlay_lg::discover_lg_tvs(std::time::Duration::from_millis(timeout_ms as u64))
            .await
            .map_err(|e| DaemonError::OperationFailed(e.to_string()))?;

        Ok(tvs
            .into_iter()
            .map(|tv| (tv.id, tv.name, tv.ip, tv.mac))
            .collect())
    }

    /// Pair a new TV.
    ///
    /// Connects to the TV at the given IP, triggering a pairing prompt on the TV.
    /// User must accept the prompt on the TV for pairing to succeed.
    ///
    /// Arguments:
    /// - `ip`: TV IP address
    /// - `name`: Display name for the TV
    /// - `mac`: MAC address for Wake-on-LAN (optional, empty string if not provided)
    ///
    /// Returns: TV ID
    async fn pair_tv(&self, ip: &str, name: &str, mac: &str) -> Result<String, DaemonError> {
        info!(ip = %ip, name = %name, "D-Bus: PairTv");
        let manager = self.manager.read().await;
        let mac_opt = if mac.is_empty() { None } else { Some(mac) };
        manager
            .pair_tv(ip, name, mac_opt, None)
            .await
            .map_err(DaemonError::from)
    }

    /// Remove a TV from configuration.
    async fn remove_tv(&self, tv_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, "D-Bus: RemoveTv");
        let manager = self.manager.read().await;
        manager.remove_tv(tv_id).await.map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    async fn power_on(&self, tv_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, "D-Bus: PowerOn");
        let manager = self.manager.read().await;
        manager.power_on(tv_id).await.map_err(DaemonError::from)
    }

    /// Power off a TV.
    async fn power_off(&self, tv_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, "D-Bus: PowerOff");
        let manager = self.manager.read().await;
        manager.power_off(tv_id).await.map_err(DaemonError::from)
    }

    /// Turn screen on (resume from blank).
    async fn screen_on(&self, tv_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, "D-Bus: ScreenOn");
        let manager = self.manager.read().await;
        manager.screen_on(tv_id).await.map_err(DaemonError::from)
    }

    /// Turn screen off (OLED-safe blank).
    async fn screen_off(&self, tv_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, "D-Bus: ScreenOff");
        let manager = self.manager.read().await;
        manager.screen_off(tv_id).await.map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch to a specific input.
    async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, input_id = %input_id, "D-Bus: SwitchInput");
        let manager = self.manager.read().await;
        manager
            .switch_input(tv_id, input_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Get available inputs.
    /// Returns: [(id, label)]
    async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, DaemonError> {
        debug!(tv_id = %tv_id, "D-Bus: GetInputs");
        let manager = self.manager.read().await;
        manager.get_inputs(tv_id).await.map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // Audio Control
    // -------------------------------------------------------------------------

    /// Set volume (0-100).
    async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, volume = volume, "D-Bus: SetVolume");
        let manager = self.manager.read().await;
        manager
            .set_volume(tv_id, volume)
            .await
            .map_err(DaemonError::from)
    }

    /// Get current volume.
    async fn get_volume(&self, tv_id: &str) -> Result<u32, DaemonError> {
        debug!(tv_id = %tv_id, "D-Bus: GetVolume");
        let manager = self.manager.read().await;
        manager.get_volume(tv_id).await.map_err(DaemonError::from)
    }

    /// Set mute state.
    async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, mute = mute, "D-Bus: SetMute");
        let manager = self.manager.read().await;
        manager
            .set_mute(tv_id, mute)
            .await
            .map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // Picture Control
    // -------------------------------------------------------------------------

    /// Set backlight (OLED pixel brightness, 0-100).
    async fn set_backlight(&self, tv_id: &str, backlight: u32) -> Result<(), DaemonError> {
        info!(tv_id = %tv_id, backlight = backlight, "D-Bus: SetBacklight");
        let manager = self.manager.read().await;
        manager
            .set_backlight(tv_id, backlight)
            .await
            .map_err(DaemonError::from)
    }

    /// Get current backlight.
    async fn get_backlight(&self, tv_id: &str) -> Result<u32, DaemonError> {
        debug!(tv_id = %tv_id, "D-Bus: GetBacklight");
        let manager = self.manager.read().await;
        manager
            .get_backlight(tv_id)
            .await
            .map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state as JSON.
    async fn get_state(&self, tv_id: &str) -> Result<String, DaemonError> {
        debug!(tv_id = %tv_id, "D-Bus: GetState");
        let manager = self.manager.read().await;
        let state = manager.get_state(tv_id).await.map_err(DaemonError::from)?;
        serde_json::to_string(&state)
            .map_err(|e| DaemonError::OperationFailed(format!("JSON serialization failed: {}", e)))
    }

    // -------------------------------------------------------------------------
    // Configuration
    // -------------------------------------------------------------------------

    /// Reload configuration from disk.
    ///
    /// Adds new TVs, updates existing ones, and removes TVs no longer in config.
    /// Active connections are preserved if the TV config hasn't changed.
    async fn reload_config(&self) -> Result<(), DaemonError> {
        info!("D-Bus: ReloadConfig");
        let manager = self.manager.read().await;
        manager.reload_config().await.map_err(DaemonError::from)
    }

    // -------------------------------------------------------------------------
    // Signals
    // -------------------------------------------------------------------------

    /// Signal emitted when TV state changes.
    ///
    /// Arguments:
    /// - `tv_id`: The TV identifier
    /// - `state`: JSON-encoded state object
    #[zbus(signal)]
    pub async fn tv_state_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        tv_id: &str,
        state: &str,
    ) -> zbus::Result<()>;
}
