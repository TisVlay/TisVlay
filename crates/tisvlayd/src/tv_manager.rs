//! TV connection manager.
//!
//! Manages connections to multiple TVs using vendor-agnostic trait objects.

use crate::registry::BackendRegistry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tisvlay_core::{Capabilities, Config, PowerState, TvConfig, TvConnection, TvState};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

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
    /// Display address (extracted from vendor-specific config via connector).
    pub address: String,
    pub vendor: String,
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
}

impl ManagedTv {
    fn new(config: TvConfig) -> Self {
        Self {
            config,
            connection: None,
            state: TvState::default(),
            last_error: None,
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

/// TV connection manager.
///
/// Thread-safe manager for multiple TV connections.
/// Connection resilience (auto-reconnect, WoL retry) is handled by the backends.
pub struct TvManager {
    /// Global configuration.
    config: Config,
    /// Backend registry for vendor support.
    registry: Arc<BackendRegistry>,
    /// Managed TVs by ID.
    tvs: Arc<RwLock<HashMap<String, ManagedTv>>>,
    /// Channel to send state change events.
    state_change_tx: mpsc::UnboundedSender<StateChangeEvent>,
}

impl TvManager {
    /// Create a new TV manager from configuration.
    ///
    /// Returns the manager and a receiver for state change events.
    pub fn new(
        config: Config,
        registry: Arc<BackendRegistry>,
    ) -> (Self, mpsc::UnboundedReceiver<StateChangeEvent>) {
        let mut tvs = HashMap::new();
        for tv_config in &config.tv {
            tvs.insert(tv_config.id.clone(), ManagedTv::new(tv_config.clone()));
        }

        let tvs = Arc::new(RwLock::new(tvs));
        let (state_change_tx, state_change_rx) = mpsc::unbounded_channel();

        let manager = Self {
            config,
            registry,
            tvs,
            state_change_tx,
        };

        (manager, state_change_rx)
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
            if self.registry.supports_wake(&tv.config) {
                info!(tv_id = %id, "Waking TV");
                if let Err(e) = self.registry.wake_tv(&tv.config).await {
                    warn!(tv_id = %id, error = %e, "Failed to wake TV");
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
                let address = self.registry.display_address(&tv.config);
                (id.clone(), tv.config.name.clone(), address, tv.is_connected())
            })
            .collect()
    }

    /// Get basic TV information.
    pub async fn get_tv_info(&self, tv_id: &str) -> Option<TvInfo> {
        let tvs = self.tvs.read().await;
        tvs.get(tv_id).map(|tv| {
            let address = self.registry.display_address(&tv.config);
            TvInfo {
                id: tv.config.id.clone(),
                name: tv.config.name.clone(),
                address,
                vendor: tv.config.vendor.clone(),
            }
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
        let (display_addr, config) = {
            let tvs = self.tvs.read().await;
            let tv = tvs
                .get(tv_id)
                .ok_or_else(|| format!("TV not found: {}", tv_id))?;

            if tv.is_connected() {
                return Ok(());
            }

            let display_addr = self.registry.display_address(&tv.config);
            let config = tv.config.clone();

            (display_addr, config)
        };

        info!(tv_id = %tv_id, address = %display_addr, vendor = %config.vendor, "Connecting to TV");

        // Connect using registry (no lock held)
        let connection = self
            .registry
            .connect_tv(&config)
            .await
            .map_err(|e| format!("Connection failed: {}", e))?;

        // Store connection (write lock)
        {
            let mut tvs = self.tvs.write().await;
            if let Some(tv) = tvs.get_mut(tv_id) {
                info!(tv_id = %tv_id, "Connected to TV");
                tv.connection = Some(connection);
                tv.last_error = None;
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

    /// Execute a command on a TV connection.
    ///
    /// Connects if not already connected, then executes the command.
    /// The backend handles its own reconnection internally via `ensure_connected()`.
    async fn with_connection<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut dyn TvConnection) -> std::pin::Pin<Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>>,
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

        match f(connection.as_mut()).await {
            Ok(result) => Ok(result),
            Err(e) => {
                let msg = format!("Command failed: {}", e);
                warn!(tv_id = %tv_id, error = %e, "Command failed");
                Err(msg)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    ///
    /// The backend's `wake()` handles WoL retry with network readiness
    /// internally. After waking, waits for the TV to boot then connects.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), String> {
        let config = self
            .get_tv_config(tv_id)
            .await
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        if !self.registry.supports_wake(&config) {
            return Err("Wake not supported for this TV".to_string());
        }

        let display_addr = self.registry.display_address(&config);
        info!(tv_id = %tv_id, address = %display_addr, "Waking TV");

        // Drop any stale connection
        {
            let mut tvs = self.tvs.write().await;
            if let Some(tv) = tvs.get_mut(tv_id) {
                if let Some(mut conn) = tv.connection.take() {
                    let _ = conn.close().await;
                }
            }
        }

        // Wake the TV — backend handles WoL retry with network readiness
        self.registry
            .wake_tv(&config)
            .await
            .map_err(|e| format!("Wake failed: {}", e))?;

        // Wait for TV to boot, then connect
        let delay = Duration::from_millis(self.config.daemon.wake_delay_ms);
        tokio::time::sleep(delay).await;

        // Try to connect (backend handles reconnection internally)
        self.connect(tv_id).await?;

        info!(tv_id = %tv_id, "TV is online after WoL");
        Ok(())
    }

    /// Power off a TV.
    ///
    /// After sending the power-off command, clears the daemon's connection
    /// tracking. The backend already closed its own socket.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), String> {
        let result = self
            .with_connection(tv_id, |conn| {
                Box::pin(async move { conn.power_off().await })
            })
            .await;

        // Clear connection tracking — backend already closed its socket
        {
            let mut tvs = self.tvs.write().await;
            if let Some(tv) = tvs.get_mut(tv_id) {
                tv.connection = None;
            }
        }

        result
    }

    /// Get power state.
    pub async fn get_power_state(&self, tv_id: &str) -> Result<PowerState, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_power_state().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Screen Control
    // -------------------------------------------------------------------------

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_on().await })
        })
        .await
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_off().await })
        })
        .await
    }

    /// Check if screen is on.
    pub async fn is_screen_on(&self, tv_id: &str) -> Result<bool, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.is_screen_on().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Volume Control
    // -------------------------------------------------------------------------

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<(), String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_volume(volume as u8).await })
        })
        .await
    }

    /// Get volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u32, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_volume().await.map(|v| v as u32) })
        })
        .await
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_mute(mute).await })
        })
        .await
    }

    /// Check if muted.
    pub async fn is_muted(&self, tv_id: &str) -> Result<bool, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.is_muted().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), String> {
        let input = input_id.to_string();
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.switch_input(&input).await })
        })
        .await
    }

    /// Get current input.
    pub async fn get_current_input(&self, tv_id: &str) -> Result<Option<String>, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_current_input().await })
        })
        .await
    }

    /// Get available inputs.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move {
                let inputs = conn.list_inputs().await?;
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
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_backlight(backlight as u8).await })
        })
        .await
    }

    /// Get backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u32, String> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_backlight().await.map(|v| v as u32) })
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
        name: &str,
        vendor: &str,
        vendor_config: toml::Value,
        id: Option<&str>,
    ) -> Result<String, String> {
        info!(name = %name, vendor = %vendor, "Pairing TV");

        // Start pairing via registry
        let mut session = self
            .registry
            .start_pairing(vendor, &vendor_config)
            .map_err(|e| format!("Failed to start pairing: {}", e))?;

        // Wait for pairing to complete
        let credentials = session
            .wait_for_completion()
            .await
            .map_err(|e| format!("Pairing failed: {}", e))?;

        // Build final vendor config from pairing results
        let final_config = self
            .registry
            .build_config_from_pairing(vendor, &vendor_config, &credentials)
            .map_err(|e| format!("Failed to build config from pairing: {}", e))?;

        // Generate ID from name if not provided
        let tv_id = id.map(String::from).unwrap_or_else(|| {
            name.to_lowercase()
                .replace(' ', "-")
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect()
        });

        // Create TV config with vendor-specific connection parameters
        let mut tv_config = TvConfig::new(&tv_id, name)
            .with_vendor(vendor)
            .with_profile("default");

        // Store the vendor config
        tv_config
            .vendor_config
            .insert(vendor.to_string(), final_config);

        // Connect with the new config
        let connection = self
            .registry
            .connect_tv(&tv_config)
            .await
            .map_err(|e| format!("Failed to connect after pairing: {}", e))?;

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
                // Compare vendor and vendor_config which affect connection
                let connection_changed = existing.config.vendor != tv_config.vendor
                    || existing.config.vendor_config != tv_config.vendor_config;
                if connection_changed {
                    info!(tv_id = %tv_config.id, "Updating TV config (will reconnect)");
                    if let Some(mut conn) = existing.connection.take() {
                        let _ = conn.close().await;
                    }
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
}

