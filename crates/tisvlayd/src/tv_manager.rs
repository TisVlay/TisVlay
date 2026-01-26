//! TV connection manager.
//!
//! Manages connections to multiple TVs using vendor-agnostic trait objects.

use crate::registry::BackendRegistry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tisvlay_core::{
    BacklightControl, Capabilities, Config, InputControl, PowerControl, PowerState, ScreenControl,
    TvConfig, TvConnection, TvState, VolumeControl,
};
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Event emitted when TV state changes.
#[derive(Debug, Clone)]
pub struct StateChangeEvent {
    /// The TV identifier.
    pub tv_id: String,
    /// The new state (JSON-encoded).
    pub state_json: String,
}

impl StateChangeEvent {
    /// Create a new state change event from a TvState.
    pub fn from_state(tv_id: &str, state: &TvState) -> Self {
        Self {
            tv_id: tv_id.to_string(),
            state_json: serde_json::to_string(state).unwrap_or_default(),
        }
    }

    /// Create a state change event for a connection status change.
    pub fn connection_change(tv_id: &str, connected: bool, error: Option<&str>) -> Self {
        let state = serde_json::json!({
            "connected": connected,
            "error": error,
        });
        Self {
            tv_id: tv_id.to_string(),
            state_json: state.to_string(),
        }
    }
}

/// Basic TV information for D-Bus properties.
#[derive(Debug, Clone)]
pub struct TvInfo {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub vendor: String,
}

/// Minimum reconnection delay (1 second).
const MIN_RECONNECT_DELAY_MS: u64 = 1000;

/// Reconnection state for a TV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ReconnectState {
    /// Not attempting reconnection.
    #[default]
    Idle,
    /// Reconnection in progress with current backoff delay.
    Reconnecting { delay_ms: u64 },
}

/// Managed TV connection state.
struct ManagedTv {
    /// TV configuration.
    config: TvConfig,
    /// Active connection (None if disconnected).
    connection: Option<Box<dyn TvConnection>>,
    /// Last known TV state.
    #[allow(dead_code)]
    state: TvState,
    /// Last error message.
    last_error: Option<String>,
    /// Reconnection state.
    reconnect_state: ReconnectState,
}

impl ManagedTv {
    fn new(config: TvConfig) -> Self {
        Self {
            config,
            connection: None,
            state: TvState::default(),
            last_error: None,
            reconnect_state: ReconnectState::Idle,
        }
    }

    fn is_connected(&self) -> bool {
        self.connection
            .as_ref()
            .map(|c| c.is_connected())
            .unwrap_or(false)
    }

    fn capabilities(&self) -> Capabilities {
        self.connection
            .as_ref()
            .map(|c| c.capabilities())
            .unwrap_or(Capabilities::empty())
    }
}

/// Message types for TV manager internal communication.
#[derive(Debug)]
enum ManagerMsg {
    /// Request reconnection for a TV.
    Reconnect(String),
}

/// TV connection manager.
///
/// Thread-safe manager for multiple TV connections with automatic reconnection.
pub struct TvManager {
    /// Global configuration.
    config: Config,
    /// Backend registry for vendor support.
    registry: Arc<BackendRegistry>,
    /// Managed TVs by ID.
    tvs: Arc<RwLock<HashMap<String, ManagedTv>>>,
    /// Channel to send reconnection requests.
    reconnect_tx: mpsc::Sender<ManagerMsg>,
    /// Channel to send state change events.
    state_change_tx: mpsc::UnboundedSender<StateChangeEvent>,
}

impl TvManager {
    /// Create a new TV manager from configuration.
    ///
    /// Returns the manager, a task handle for the reconnection worker, and a receiver
    /// for state change events.
    pub fn new(
        config: Config,
        registry: Arc<BackendRegistry>,
    ) -> (
        Self,
        JoinHandle<()>,
        mpsc::UnboundedReceiver<StateChangeEvent>,
    ) {
        let mut tvs = HashMap::new();
        for tv_config in &config.tv {
            tvs.insert(tv_config.id.clone(), ManagedTv::new(tv_config.clone()));
        }

        let tvs = Arc::new(RwLock::new(tvs));
        let (reconnect_tx, reconnect_rx) = mpsc::channel(32);
        let (state_change_tx, state_change_rx) = mpsc::unbounded_channel();

        // Spawn reconnection worker
        let worker_tvs = tvs.clone();
        let worker_registry = registry.clone();
        let worker_state_tx = state_change_tx.clone();
        let max_delay = config.daemon.reconnect_interval_ms;
        let worker_handle = tokio::spawn(async move {
            reconnect_worker(
                worker_tvs,
                worker_registry,
                reconnect_rx,
                max_delay,
                worker_state_tx,
            )
            .await;
        });

        let manager = Self {
            config,
            registry,
            tvs,
            reconnect_tx,
            state_change_tx,
        };

        (manager, worker_handle, state_change_rx)
    }

    /// Emit a state change event.
    fn emit_state_change(&self, event: StateChangeEvent) {
        if let Err(e) = self.state_change_tx.send(event) {
            debug!("Failed to send state change event: {}", e);
        }
    }

    /// Wake TVs that have wake_on_boot enabled.
    pub async fn boot_wake(&self) {
        if !self.config.daemon.wake_on_boot {
            debug!("Boot-time WoL disabled");
            return;
        }

        info!("Performing boot-time Wake-on-LAN");
        let tvs = self.tvs.read().await;

        for (id, tv) in tvs.iter() {
            let mac = &tv.config.mac;
            if !mac.is_empty() {
                info!(tv_id = %id, mac = %mac, "Sending WoL packet");
                match tisvlay_core::MacAddress::parse(mac) {
                    Ok(mac_addr) => {
                        if let Err(e) = tisvlay_core::wol::wake(&mac_addr) {
                            warn!(tv_id = %id, error = %e, "Failed to send WoL");
                        }
                    }
                    Err(e) => {
                        warn!(tv_id = %id, error = %e, "Invalid MAC address");
                    }
                }
            }
        }

        // Wait for TVs to wake up
        let delay = Duration::from_millis(self.config.daemon.wake_delay_ms);
        info!("Waiting {:?} for TVs to wake", delay);
        tokio::time::sleep(delay).await;
    }

    /// List all configured TV IDs.
    pub async fn list_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        tvs.keys().cloned().collect()
    }

    /// List all configured TVs with connection status.
    pub async fn list_tvs(&self) -> Vec<(String, String, String, bool)> {
        let tvs = self.tvs.read().await;
        tvs.iter()
            .map(|(id, tv)| {
                (
                    id.clone(),
                    tv.config.name.clone(),
                    tv.config.ip.clone(),
                    tv.is_connected(),
                )
            })
            .collect()
    }

    /// Get basic TV information.
    pub async fn get_tv_info(&self, tv_id: &str) -> Option<TvInfo> {
        let tvs = self.tvs.read().await;
        tvs.get(tv_id).map(|tv| TvInfo {
            id: tv.config.id.clone(),
            name: tv.config.name.clone(),
            ip: tv.config.ip.clone(),
            vendor: tv.config.vendor.clone(),
        })
    }

    /// Check if a TV is connected.
    pub async fn is_connected(&self, tv_id: &str) -> bool {
        let tvs = self.tvs.read().await;
        tvs.get(tv_id).map(|tv| tv.is_connected()).unwrap_or(false)
    }

    /// Get TV capabilities.
    pub async fn get_capabilities(&self, tv_id: &str) -> Option<Capabilities> {
        let tvs = self.tvs.read().await;
        tvs.get(tv_id).map(|tv| tv.capabilities())
    }

    /// Get TV configuration by ID.
    pub async fn get_tv_config(&self, tv_id: &str) -> Option<TvConfig> {
        let tvs = self.tvs.read().await;
        tvs.get(tv_id).map(|tv| tv.config.clone())
    }

    /// Connect to a TV, returning success/failure.
    async fn connect(&self, tv_id: &str) -> Result<(), String> {
        // Get config first (read lock)
        let (ip, vendor, vendor_config) = {
            let tvs = self.tvs.read().await;
            let tv = tvs
                .get(tv_id)
                .ok_or_else(|| format!("TV not found: {}", tv_id))?;

            if tv.is_connected() {
                return Ok(());
            }

            let vendor_config: Box<dyn std::any::Any + Send + Sync> =
                if let Some(lg_config) = &tv.config.lg {
                    Box::new(tisvlay_lg::LgConfig {
                        client_key: lg_config.client_key.clone(),
                        default_input: lg_config.default_input.clone(),
                    })
                } else {
                    Box::new(tisvlay_lg::LgConfig::default())
                };

            (tv.config.ip.clone(), tv.config.vendor.clone(), vendor_config)
        };

        info!(tv_id = %tv_id, ip = %ip, vendor = %vendor, "Connecting to TV");

        // Connect using registry (no lock held)
        let connection = self
            .registry
            .connect(&vendor, &ip, None, vendor_config.as_ref())
            .await
            .map_err(|e| format!("Connection failed: {}", e))?;

        // Store connection (write lock)
        {
            let mut tvs = self.tvs.write().await;
            if let Some(tv) = tvs.get_mut(tv_id) {
                info!(tv_id = %tv_id, "Connected to TV");
                tv.connection = Some(connection);
                tv.last_error = None;
                tv.reconnect_state = ReconnectState::Idle;
            }
        }

        // Emit connection state change
        let event = StateChangeEvent::connection_change(tv_id, true, None);
        self.emit_state_change(event);

        Ok(())
    }

    /// Force reconnection to a TV.
    pub async fn reconnect(&self, tv_id: &str) -> Result<(), String> {
        // Disconnect first
        {
            let mut tvs = self.tvs.write().await;
            if let Some(tv) = tvs.get_mut(tv_id) {
                if let Some(mut conn) = tv.connection.take() {
                    let _ = conn.close().await;
                }
            }
        }

        // Then connect
        self.connect(tv_id).await
    }

    /// Trigger a reconnection attempt for a TV.
    async fn trigger_reconnect(&self, tv_id: &str) {
        // Check if reconnection is already in progress
        {
            let tvs = self.tvs.read().await;
            if let Some(tv) = tvs.get(tv_id) {
                if tv.reconnect_state != ReconnectState::Idle {
                    debug!(tv_id = %tv_id, "Reconnection already in progress");
                    return;
                }
            }
        }

        // Mark as reconnecting
        {
            let mut tvs = self.tvs.write().await;
            if let Some(tv) = tvs.get_mut(tv_id) {
                tv.reconnect_state = ReconnectState::Reconnecting {
                    delay_ms: MIN_RECONNECT_DELAY_MS,
                };
            }
        }

        // Send reconnection request to worker
        if let Err(e) = self
            .reconnect_tx
            .send(ManagerMsg::Reconnect(tv_id.to_string()))
            .await
        {
            error!(tv_id = %tv_id, error = %e, "Failed to send reconnect request");
        }
    }

    /// Execute a command on a TV connection.
    ///
    /// Connects if not already connected, then executes the command.
    async fn with_connection<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut dyn TvConnection) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
    {
        // First try to connect if needed
        self.connect(tv_id).await?;

        // Then execute the command
        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        match f(connection.as_mut()).await {
            Ok(result) => Ok(result),
            Err(e) => {
                let msg = format!("Command failed: {}", e);
                error!(tv_id = %tv_id, error = %e, "Command failed");
                // Mark as disconnected on error
                tv.connection = None;
                tv.last_error = Some(msg.clone());

                // Emit disconnection state change
                let event = StateChangeEvent::connection_change(tv_id, false, Some(&msg));
                let tv_id_owned = tv_id.to_string();
                drop(tvs);

                self.emit_state_change(event);
                self.trigger_reconnect(&tv_id_owned).await;

                Err(msg)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), String> {
        let config = self
            .get_tv_config(tv_id)
            .await
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let mac = &config.mac;
        if mac.is_empty() {
            return Err("MAC address not configured for Wake-on-LAN".to_string());
        }

        info!(tv_id = %tv_id, mac = %mac, "Sending WoL packet");
        let mac_addr = tisvlay_core::MacAddress::parse(mac)
            .map_err(|e| format!("Invalid MAC address: {}", e))?;
        tisvlay_core::wol::wake(&mac_addr).map_err(|e| format!("WoL failed: {}", e))?;

        // Wait for TV to wake
        let delay = Duration::from_millis(self.config.daemon.wake_delay_ms);
        tokio::time::sleep(delay).await;

        // Try to connect
        self.connect(tv_id).await?;

        // Switch to default input if configured (LG-specific)
        if let Some(ref lg) = config.lg {
            if let Some(ref default_input) = lg.default_input {
                info!(tv_id = %tv_id, input = %default_input, "Switching to default input");
                let _ = self.switch_input(tv_id, default_input).await;
            }
        }

        Ok(())
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_power_control(tv_id, |power| {
            Box::pin(async move { power.power_off().await })
        })
        .await
    }

    /// Get power state.
    pub async fn get_power_state(&self, tv_id: &str) -> Result<PowerState, String> {
        self.with_power_control(tv_id, |power| {
            Box::pin(async move { power.get_power_state().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Screen Control
    // -------------------------------------------------------------------------

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), String> {
        self.with_screen_control(tv_id, |screen| {
            Box::pin(async move { screen.screen_on().await })
        })
        .await
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_screen_control(tv_id, |screen| {
            Box::pin(async move { screen.screen_off().await })
        })
        .await
    }

    /// Check if screen is on.
    pub async fn is_screen_on(&self, tv_id: &str) -> Result<bool, String> {
        self.with_screen_control(tv_id, |screen| {
            Box::pin(async move { screen.is_screen_on().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Volume Control
    // -------------------------------------------------------------------------

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<(), String> {
        self.with_volume_control(tv_id, |vol| {
            Box::pin(async move { vol.set_volume(volume as u8).await })
        })
        .await
    }

    /// Get volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u32, String> {
        self.with_volume_control(tv_id, |vol| {
            Box::pin(async move { vol.get_volume().await.map(|v| v as u32) })
        })
        .await
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), String> {
        self.with_volume_control(tv_id, |vol| {
            Box::pin(async move { vol.set_mute(mute).await })
        })
        .await
    }

    /// Check if muted.
    pub async fn is_muted(&self, tv_id: &str) -> Result<bool, String> {
        self.with_volume_control(tv_id, |vol| {
            Box::pin(async move { vol.is_muted().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), String> {
        let input = input_id.to_string();
        self.with_input_control(tv_id, |inp| {
            Box::pin(async move { inp.switch_input(&input).await })
        })
        .await
    }

    /// Get current input.
    pub async fn get_current_input(&self, tv_id: &str) -> Result<Option<String>, String> {
        self.with_input_control(tv_id, |inp| {
            Box::pin(async move { inp.get_current_input().await })
        })
        .await
    }

    /// Get available inputs.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, String> {
        self.with_input_control(tv_id, |inp| {
            Box::pin(async move {
                let inputs = inp.list_inputs().await?;
                Ok(inputs.into_iter().map(|i| (i.id, i.label)).collect())
            })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Backlight Control
    // -------------------------------------------------------------------------

    /// Set backlight.
    pub async fn set_backlight(&self, tv_id: &str, backlight: u32) -> Result<(), String> {
        self.with_backlight_control(tv_id, |bl| {
            Box::pin(async move { bl.set_backlight(backlight as u8).await })
        })
        .await
    }

    /// Get backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u32, String> {
        self.with_backlight_control(tv_id, |bl| {
            Box::pin(async move { bl.get_backlight().await.map(|v| v as u32) })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state.
    pub async fn get_state(&self, tv_id: &str) -> Result<TvState, String> {
        // First try to connect if needed
        self.connect(tv_id).await?;

        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        connection
            .get_state()
            .await
            .map_err(|e| format!("Failed to get state: {}", e))
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// Pair a new TV.
    pub async fn pair_tv(
        &self,
        ip: &str,
        name: &str,
        vendor: &str,
        mac: Option<&str>,
        id: Option<&str>,
    ) -> Result<String, String> {
        info!(ip = %ip, name = %name, vendor = %vendor, "Pairing TV");

        // Connect to TV using registry (this triggers pairing prompt)
        let backend = self
            .registry
            .get(vendor)
            .ok_or_else(|| format!("Unknown vendor: {}", vendor))?;

        // For LG, we need an empty config to trigger pairing
        let empty_config = tisvlay_lg::LgConfig::default();
        let connection = backend
            .connector
            .connect(ip, mac, &empty_config)
            .await
            .map_err(|e| format!("Failed to connect to TV: {}", e))?;

        // Get client key from the connection (vendor-specific)
        let client_key = if vendor == "lg" {
            // Downcast to LgConnection to get the client key
            if let Some(lg_conn) = connection.as_any().downcast_ref::<tisvlay_lg::LgConnection>() {
                lg_conn.client().client_key().map(String::from)
            } else {
                None
            }
        } else {
            None
        };

        // Generate ID from name if not provided
        let tv_id = id.map(String::from).unwrap_or_else(|| {
            name.to_lowercase()
                .replace(' ', "-")
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect()
        });

        // Create TV config
        let tv_config = TvConfig {
            id: tv_id.clone(),
            name: name.to_string(),
            ip: ip.to_string(),
            mac: mac.unwrap_or("").to_string(),
            vendor: vendor.to_string(),
            profile: "default".to_string(),
            hdmi_input: None,
            only_when_active: false,
            lg: if vendor == "lg" {
                Some(tisvlay_core::LgVendorConfig {
                    client_key,
                    default_input: None,
                })
            } else {
                None
            },
        };

        // Add to managed TVs
        {
            let mut tvs = self.tvs.write().await;
            let mut managed = ManagedTv::new(tv_config.clone());
            managed.connection = Some(connection);
            tvs.insert(tv_id.clone(), managed);
        }

        // Save config
        self.save_tv_config(&tv_config)?;

        info!(tv_id = %tv_id, "TV paired successfully");
        Ok(tv_id)
    }

    /// Remove a TV.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<(), String> {
        info!(tv_id = %tv_id, "Removing TV");

        // Remove from managed TVs
        let removed = {
            let mut tvs = self.tvs.write().await;
            if let Some(mut tv) = tvs.remove(tv_id) {
                // Close connection if any
                if let Some(mut conn) = tv.connection.take() {
                    let _ = conn.close().await;
                }
                true
            } else {
                false
            }
        };

        if !removed {
            return Err(format!("TV not found: {}", tv_id));
        }

        // Remove from config file
        self.delete_tv_config(tv_id)?;

        info!(tv_id = %tv_id, "TV removed successfully");
        Ok(())
    }

    /// Save a TV config to the configuration file.
    fn save_tv_config(&self, tv_config: &TvConfig) -> Result<(), String> {
        let mut config = Config::load().map_err(|e| format!("Failed to load config: {}", e))?;
        config.upsert_tv(tv_config.clone());
        config
            .save()
            .map_err(|e| format!("Failed to save config: {}", e))
    }

    /// Delete a TV from the configuration file.
    fn delete_tv_config(&self, tv_id: &str) -> Result<(), String> {
        let mut config = Config::load().map_err(|e| format!("Failed to load config: {}", e))?;
        config.remove_tv(tv_id);
        config
            .save()
            .map_err(|e| format!("Failed to save config: {}", e))
    }

    /// Reload configuration from disk.
    pub async fn reload_config(&self) -> Result<(), String> {
        info!("Reloading configuration");

        let new_config = Config::load().map_err(|e| format!("Failed to load config: {}", e))?;

        let mut tvs = self.tvs.write().await;

        // Track IDs from new config
        let new_ids: std::collections::HashSet<_> =
            new_config.tv.iter().map(|t| t.id.clone()).collect();

        // Remove TVs that are no longer in config
        let old_ids: Vec<_> = tvs.keys().cloned().collect();
        for old_id in old_ids {
            if !new_ids.contains(&old_id) {
                info!(tv_id = %old_id, "Removing TV (no longer in config)");
                if let Some(mut tv) = tvs.remove(&old_id) {
                    if let Some(mut conn) = tv.connection.take() {
                        let _ = conn.close().await;
                    }
                }
            }
        }

        // Add or update TVs from new config
        for tv_config in new_config.tv {
            if let Some(existing) = tvs.get_mut(&tv_config.id) {
                // Update config if changed (preserve connection if possible)
                let ip_changed = existing.config.ip != tv_config.ip;
                let lg_changed = existing.config.lg != tv_config.lg;
                if ip_changed || lg_changed {
                    info!(tv_id = %tv_config.id, "Updating TV config (will reconnect)");
                    if let Some(mut conn) = existing.connection.take() {
                        let _ = conn.close().await;
                    }
                    existing.reconnect_state = ReconnectState::Idle;
                }
                existing.config = tv_config;
            } else {
                // Add new TV
                info!(tv_id = %tv_config.id, "Adding new TV from config");
                tvs.insert(tv_config.id.clone(), ManagedTv::new(tv_config));
            }
        }

        info!(tv_count = tvs.len(), "Configuration reloaded");
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Helper methods for trait-based control
    // -------------------------------------------------------------------------

    async fn with_power_control<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut (dyn PowerControl + Send)) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
    {
        self.connect(tv_id).await?;

        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        // Downcast to LgConnection which implements PowerControl
        if let Some(lg_conn) = connection.as_any_mut().downcast_mut::<tisvlay_lg::LgConnection>() {
            f(lg_conn).await.map_err(|e| e.to_string())
        } else {
            Err("Power control not supported for this TV type".to_string())
        }
    }

    async fn with_screen_control<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut (dyn ScreenControl + Send)) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
    {
        self.connect(tv_id).await?;

        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        if let Some(lg_conn) = connection.as_any_mut().downcast_mut::<tisvlay_lg::LgConnection>() {
            f(lg_conn).await.map_err(|e| e.to_string())
        } else {
            Err("Screen control not supported for this TV type".to_string())
        }
    }

    async fn with_volume_control<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut (dyn VolumeControl + Send)) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
    {
        self.connect(tv_id).await?;

        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        if let Some(lg_conn) = connection.as_any_mut().downcast_mut::<tisvlay_lg::LgConnection>() {
            f(lg_conn).await.map_err(|e| e.to_string())
        } else {
            Err("Volume control not supported for this TV type".to_string())
        }
    }

    async fn with_input_control<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut (dyn InputControl + Send)) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
    {
        self.connect(tv_id).await?;

        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        if let Some(lg_conn) = connection.as_any_mut().downcast_mut::<tisvlay_lg::LgConnection>() {
            f(lg_conn).await.map_err(|e| e.to_string())
        } else {
            Err("Input control not supported for this TV type".to_string())
        }
    }

    async fn with_backlight_control<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut (dyn BacklightControl + Send)) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
    {
        self.connect(tv_id).await?;

        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        if let Some(lg_conn) = connection.as_any_mut().downcast_mut::<tisvlay_lg::LgConnection>() {
            f(lg_conn).await.map_err(|e| e.to_string())
        } else {
            Err("Backlight control not supported for this TV type".to_string())
        }
    }
}

/// Background worker for handling reconnection attempts with exponential backoff.
async fn reconnect_worker(
    tvs: Arc<RwLock<HashMap<String, ManagedTv>>>,
    registry: Arc<BackendRegistry>,
    mut rx: mpsc::Receiver<ManagerMsg>,
    max_delay_ms: u64,
    state_change_tx: mpsc::UnboundedSender<StateChangeEvent>,
) {
    info!("Reconnection worker started");

    while let Some(msg) = rx.recv().await {
        match msg {
            ManagerMsg::Reconnect(tv_id) => {
                reconnect_tv(&tvs, &registry, &tv_id, max_delay_ms, &state_change_tx).await;
            }
        }
    }

    info!("Reconnection worker stopped");
}

/// Attempt to reconnect a TV with exponential backoff.
async fn reconnect_tv(
    tvs: &Arc<RwLock<HashMap<String, ManagedTv>>>,
    registry: &Arc<BackendRegistry>,
    tv_id: &str,
    max_delay_ms: u64,
    state_change_tx: &mpsc::UnboundedSender<StateChangeEvent>,
) {
    let mut current_delay_ms = MIN_RECONNECT_DELAY_MS;

    loop {
        // Get current reconnection state
        let (should_reconnect, ip, vendor, vendor_config) = {
            let tvs = tvs.read().await;
            match tvs.get(tv_id) {
                Some(tv) => {
                    // Stop if already connected or reconnection was cancelled
                    if tv.is_connected() || tv.reconnect_state == ReconnectState::Idle {
                        debug!(tv_id = %tv_id, "Reconnection no longer needed");
                        return;
                    }

                    let vendor_config: Box<dyn std::any::Any + Send + Sync> =
                        if let Some(lg_config) = &tv.config.lg {
                            Box::new(tisvlay_lg::LgConfig {
                                client_key: lg_config.client_key.clone(),
                                default_input: lg_config.default_input.clone(),
                            })
                        } else {
                            Box::new(tisvlay_lg::LgConfig::default())
                        };

                    (
                        true,
                        tv.config.ip.clone(),
                        tv.config.vendor.clone(),
                        vendor_config,
                    )
                }
                None => {
                    warn!(tv_id = %tv_id, "TV not found, stopping reconnection");
                    return;
                }
            }
        };

        if !should_reconnect {
            return;
        }

        // Wait before attempting
        info!(
            tv_id = %tv_id,
            delay_ms = current_delay_ms,
            "Waiting before reconnection attempt"
        );
        tokio::time::sleep(Duration::from_millis(current_delay_ms)).await;

        // Attempt connection
        info!(tv_id = %tv_id, ip = %ip, "Attempting reconnection");
        match registry
            .connect(&vendor, &ip, None, vendor_config.as_ref())
            .await
        {
            Ok(connection) => {
                info!(tv_id = %tv_id, "Reconnection successful");
                {
                    let mut tvs = tvs.write().await;
                    if let Some(tv) = tvs.get_mut(tv_id) {
                        tv.connection = Some(connection);
                        tv.last_error = None;
                        tv.reconnect_state = ReconnectState::Idle;
                    }
                }
                // Emit connection state change
                let event = StateChangeEvent::connection_change(tv_id, true, None);
                let _ = state_change_tx.send(event);
                return;
            }
            Err(e) => {
                warn!(tv_id = %tv_id, error = %e, "Reconnection failed");

                // Update state with new delay
                let mut tvs = tvs.write().await;
                if let Some(tv) = tvs.get_mut(tv_id) {
                    tv.last_error = Some(format!("Reconnection failed: {}", e));

                    // Calculate next delay with exponential backoff
                    current_delay_ms = (current_delay_ms * 2).min(max_delay_ms);
                    tv.reconnect_state = ReconnectState::Reconnecting {
                        delay_ms: current_delay_ms,
                    };
                }
            }
        }
    }
}
