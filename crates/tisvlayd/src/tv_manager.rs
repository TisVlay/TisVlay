//! TV connection manager.
//!
//! Manages connections to multiple TVs using vendor-agnostic trait objects.
//!
//! Uses per-TV locking so that operations on different TVs can proceed
//! concurrently. The outer `RwLock<HashMap>` is held only briefly for
//! lookup; network I/O runs under a per-TV `tokio::sync::Mutex`.

use crate::registry::BackendRegistry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tisvlay_core::{Capabilities, Config, PowerState, TvConfig, TvConnection, TvState};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

/// Error type for TV manager operations.
#[derive(Debug, thiserror::Error)]
pub enum TvManagerError {
    /// TV not found in the managed set.
    #[error("TV not found: {0}")]
    TvNotFound(String),

    /// TV exists but is not currently connected.
    #[error("Not connected to TV: {0}")]
    NotConnected(String),

    /// TV is intentionally powered off (no reconnection attempts).
    #[error("TV {0} is powered off")]
    PoweredOff(String),

    /// Wake-on-LAN failed for a TV.
    #[error("Wake failed for TV {tv_id}: {source}")]
    WakeFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// Connection to a TV failed.
    #[error("Connection failed for TV {tv_id}: {source}")]
    ConnectionFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// A command executed on a TV connection failed.
    #[error("Command failed on TV {tv_id}: {source}")]
    CommandFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// Wake not supported for this TV.
    #[error("Wake not supported for TV: {0}")]
    WakeNotSupported(String),

    /// Pairing failed.
    #[error("Pairing failed: {0}")]
    PairingFailed(Box<dyn std::error::Error + Send + Sync>),

    /// Configuration I/O error (load/save).
    #[error("Config error: {0}")]
    ConfigError(String),
}

/// Event emitted when TV state changes.
#[derive(Debug, Clone)]
pub struct StateChangeEvent {
    /// The TV identifier.
    pub tv_id: String,
    /// The new state (JSON-encoded).
    pub state_json: String,
}

impl StateChangeEvent {
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
    /// TV was intentionally powered off -- don't try to reconnect until power_on.
    powered_off: bool,
}

impl ManagedTv {
    fn new(config: TvConfig) -> Self {
        Self {
            config,
            connection: None,
            powered_off: false,
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
/// Uses per-TV locking so operations on different TVs proceed concurrently.
/// Connection resilience (auto-reconnect, WoL retry) is handled by the backends.
pub struct TvManager {
    /// Global configuration.
    config: Config,
    /// Backend registry for vendor support.
    registry: Arc<BackendRegistry>,
    /// Managed TVs by ID -- each TV has its own mutex for independent locking.
    /// The outer RwLock protects the HashMap itself (add/remove); individual TV
    /// operations hold only the per-TV Mutex.
    tvs: Arc<RwLock<HashMap<String, Arc<Mutex<ManagedTv>>>>>,
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
            tvs.insert(
                tv_config.id.clone(),
                Arc::new(Mutex::new(ManagedTv::new(tv_config.clone()))),
            );
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

    /// Get a per-TV Arc, briefly holding the outer read lock for lookup only.
    async fn get_tv(&self, tv_id: &str) -> Result<Arc<Mutex<ManagedTv>>, TvManagerError> {
        self.tvs
            .read()
            .await
            .get(tv_id)
            .cloned()
            .ok_or_else(|| TvManagerError::TvNotFound(tv_id.to_string()))
    }

    /// Wake TVs that have wake_on_boot enabled.
    pub async fn boot_wake(&self) {
        if !self.config.daemon.wake_on_boot {
            debug!("Boot-time WoL disabled");
            return;
        }

        info!("Performing boot-time Wake-on-LAN");

        // Collect configs under lock, then wake outside the lock to avoid
        // holding per-TV mutexes during network I/O (WoL can retry for seconds).
        let mut wake_configs: Vec<(String, TvConfig)> = Vec::new();
        {
            let tvs = self.tvs.read().await;
            for (id, tv_arc) in tvs.iter() {
                let tv = tv_arc.lock().await;
                if self.registry.supports_wake(&tv.config) {
                    wake_configs.push((id.clone(), tv.config.clone()));
                }
            }
        }

        for (id, config) in &wake_configs {
            info!(tv_id = %id, "Waking TV");
            if let Err(e) = self.registry.wake_tv(config).await {
                warn!(tv_id = %id, error = %e, "Failed to wake TV");
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

    /// Get basic TV information.
    pub async fn get_tv_info(&self, tv_id: &str) -> Option<TvInfo> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        let address = self.registry.display_address(&tv.config);
        Some(TvInfo {
            name: tv.config.name.clone(),
            address,
            vendor: tv.config.vendor.clone(),
        })
    }

    /// Check if a TV is connected.
    pub async fn is_connected(&self, tv_id: &str) -> bool {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => {
                let tv = tv_arc.lock().await;
                tv.is_connected()
            }
            Err(_) => false,
        }
    }

    /// Get TV capabilities.
    pub async fn get_capabilities(&self, tv_id: &str) -> Option<Capabilities> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.capabilities())
    }

    /// Force reconnection to a TV.
    ///
    /// Disconnects (if connected), then reconnects under a single per-TV lock.
    pub async fn reconnect(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;
        let mut tv = tv_arc.lock().await;

        // Disconnect if connected
        if let Some(mut conn) = tv.connection.take() {
            let _ = conn.close().await;
        }

        if tv.powered_off {
            return Err(TvManagerError::PoweredOff(tv_id.to_string()));
        }

        let display_addr = self.registry.display_address(&tv.config);
        info!(tv_id = %tv_id, address = %display_addr, vendor = %tv.config.vendor, "Reconnecting to TV");

        let connection = self.registry.connect_tv(&tv.config).await.map_err(|e| {
            TvManagerError::ConnectionFailed {
                tv_id: tv_id.to_string(),
                source: e,
            }
        })?;

        info!(tv_id = %tv_id, "Connected to TV");
        tv.connection = Some(connection);

        drop(tv);
        self.emit_state_change(StateChangeEvent::connection_change(tv_id, true, None));

        Ok(())
    }

    /// Execute a command on a TV connection.
    ///
    /// Connects if not already connected, then executes the command.
    /// Only holds the per-TV mutex -- other TVs are not blocked.
    /// The connect and command execute under a single lock acquisition
    /// to avoid TOCTOU races between connect and command.
    async fn with_connection<F, T>(&self, tv_id: &str, f: F) -> Result<T, TvManagerError>
    where
        F: FnOnce(
            &mut dyn TvConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>,
        >,
    {
        let tv_arc = self.get_tv(tv_id).await?;
        let mut tv = tv_arc.lock().await;

        // Connect if needed (under the same lock to prevent TOCTOU)
        if !tv.is_connected() {
            if tv.powered_off {
                return Err(TvManagerError::PoweredOff(tv_id.to_string()));
            }

            let display_addr = self.registry.display_address(&tv.config);
            info!(tv_id = %tv_id, address = %display_addr, vendor = %tv.config.vendor, "Connecting to TV");

            let connection = self.registry.connect_tv(&tv.config).await.map_err(|e| {
                TvManagerError::ConnectionFailed {
                    tv_id: tv_id.to_string(),
                    source: e,
                }
            })?;

            info!(tv_id = %tv_id, "Connected to TV");
            tv.connection = Some(connection);
            self.emit_state_change(StateChangeEvent::connection_change(tv_id, true, None));
        }

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| TvManagerError::NotConnected(tv_id.to_string()))?;

        match f(connection.as_mut()).await {
            Ok(result) => Ok(result),
            Err(e) => {
                warn!(tv_id = %tv_id, error = %e, "Command failed");

                // Drop stale connection so the next call reconnects
                if matches!(
                    e,
                    tisvlay_core::Error::ConnectionClosed
                        | tisvlay_core::Error::Connection(_)
                        | tisvlay_core::Error::Io(_)
                        | tisvlay_core::Error::RequestTimeout
                ) {
                    tv.connection = None;
                }

                Err(TvManagerError::CommandFailed {
                    tv_id: tv_id.to_string(),
                    source: e,
                })
            }
        }
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    ///
    /// The backend handles all transport-specific retry logic (WoL resend,
    /// boot wait, connect retry) via `wake_and_connect()`. The daemon
    /// provides timing policy from config.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;

        // Read config and clear state under per-TV lock
        let config = {
            let mut tv = tv_arc.lock().await;

            if !self.registry.supports_wake(&tv.config) {
                return Err(TvManagerError::WakeNotSupported(tv_id.to_string()));
            }

            let display_addr = self.registry.display_address(&tv.config);
            info!(tv_id = %tv_id, address = %display_addr, "Waking TV");

            // Drop any stale connection and clear the powered-off flag
            tv.powered_off = false;
            if let Some(mut conn) = tv.connection.take() {
                let _ = conn.close().await;
            }

            tv.config.clone()
        };

        // Backend handles wake + boot wait + connect retry (no lock held)
        let connection = self
            .registry
            .wake_and_connect_tv(&config, self.config.daemon.wake_delay_ms)
            .await
            .map_err(|e| TvManagerError::WakeFailed {
                tv_id: tv_id.to_string(),
                source: e,
            })?;

        // Store the connection under per-TV lock
        {
            let mut tv = tv_arc.lock().await;
            tv.connection = Some(connection);
        }

        let event = StateChangeEvent::connection_change(tv_id, true, None);
        self.emit_state_change(event);
        info!(tv_id = %tv_id, "TV is online after WoL");
        Ok(())
    }

    /// Power off a TV.
    ///
    /// After sending the power-off command, clears the daemon's connection
    /// tracking. The backend already closed its own socket.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let result = self
            .with_connection(tv_id, |conn| {
                Box::pin(async move { conn.power_off().await })
            })
            .await;

        // Clear connection tracking and mark as powered off so subsequent
        // connect() calls (e.g., from ScreenLocked racing after sleep) don't
        // try to connect to a dead TV.
        {
            let tv_arc = self.get_tv(tv_id).await?;
            let mut tv = tv_arc.lock().await;
            tv.connection = None;
            tv.powered_off = true;
        }

        result
    }

    /// Get power state.
    pub async fn get_power_state(&self, tv_id: &str) -> Result<PowerState, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_power_state().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Screen Control
    // -------------------------------------------------------------------------

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_on().await })
        })
        .await
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_off().await })
        })
        .await
    }

    /// Check if screen is on.
    pub async fn is_screen_on(&self, tv_id: &str) -> Result<bool, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.is_screen_on().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Volume Control
    // -------------------------------------------------------------------------

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_volume(volume).await })
        })
        .await
    }

    /// Get volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u8, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_volume().await })
        })
        .await
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_mute(mute).await })
        })
        .await
    }

    /// Check if muted.
    pub async fn is_muted(&self, tv_id: &str) -> Result<bool, TvManagerError> {
        self.with_connection(tv_id, |conn| Box::pin(async move { conn.is_muted().await }))
            .await
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), TvManagerError> {
        let input = input_id.to_string();
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.switch_input(&input).await })
        })
        .await
    }

    /// Get current input.
    pub async fn get_current_input(&self, tv_id: &str) -> Result<Option<String>, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_current_input().await })
        })
        .await
    }

    /// Get available inputs.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, TvManagerError> {
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
    pub async fn set_backlight(&self, tv_id: &str, backlight: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_backlight(backlight).await })
        })
        .await
    }

    /// Get backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u8, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_backlight().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state.
    pub async fn get_state(&self, tv_id: &str) -> Result<TvState, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_state().await })
        })
        .await
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
    ) -> Result<String, TvManagerError> {
        info!(name = %name, vendor = %vendor, "Pairing TV");

        // Start pairing via registry
        let mut session = self
            .registry
            .start_pairing(vendor, &vendor_config)
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        // Wait for pairing to complete
        let credentials = session
            .wait_for_completion()
            .await
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        // Build final vendor config from pairing results
        let final_config = self
            .registry
            .build_config_from_pairing(vendor, &vendor_config, &credentials)
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

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
        let connection = self.registry.connect_tv(&tv_config).await.map_err(|e| {
            TvManagerError::ConnectionFailed {
                tv_id: tv_id.clone(),
                source: e,
            }
        })?;

        // Add to managed TVs (outer write lock -- modifying the HashMap)
        {
            let mut tvs = self.tvs.write().await;
            let mut managed = ManagedTv::new(tv_config.clone());
            managed.connection = Some(connection);
            tvs.insert(tv_id.clone(), Arc::new(Mutex::new(managed)));
        }

        // Save config
        self.save_tv_config(&tv_config)?;

        info!(tv_id = %tv_id, "TV paired successfully");
        Ok(tv_id)
    }

    /// Remove a TV.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<(), TvManagerError> {
        info!(tv_id = %tv_id, "Removing TV");

        // Remove from managed TVs (outer write lock -- modifying the HashMap)
        let removed_arc = {
            let mut tvs = self.tvs.write().await;
            tvs.remove(tv_id)
        };

        match removed_arc {
            Some(tv_arc) => {
                // Close connection if any (per-TV lock)
                let mut tv = tv_arc.lock().await;
                if let Some(mut conn) = tv.connection.take() {
                    let _ = conn.close().await;
                }
            }
            None => {
                return Err(TvManagerError::TvNotFound(tv_id.to_string()));
            }
        }

        // Remove from config file
        self.delete_tv_config(tv_id)?;

        info!(tv_id = %tv_id, "TV removed successfully");
        Ok(())
    }

    /// Save a TV config to the configuration file.
    fn save_tv_config(&self, tv_config: &TvConfig) -> Result<(), TvManagerError> {
        let mut config = Config::load()
            .map_err(|e| TvManagerError::ConfigError(format!("Failed to load config: {}", e)))?;
        config.upsert_tv(tv_config.clone());
        config
            .save()
            .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
    }

    /// Delete a TV from the configuration file.
    fn delete_tv_config(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let mut config = Config::load()
            .map_err(|e| TvManagerError::ConfigError(format!("Failed to load config: {}", e)))?;
        config.remove_tv(tv_id);
        config
            .save()
            .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
    }

    /// Reload configuration from disk.
    pub async fn reload_config(&self) -> Result<(), TvManagerError> {
        info!("Reloading configuration");

        let new_config = Config::load()
            .map_err(|e| TvManagerError::ConfigError(format!("Failed to load config: {}", e)))?;

        // Outer write lock -- we may add/remove entries from the HashMap
        let mut tvs = self.tvs.write().await;

        // Track IDs from new config
        let new_ids: std::collections::HashSet<_> =
            new_config.tv.iter().map(|t| t.id.clone()).collect();

        // Remove TVs that are no longer in config
        let old_ids: Vec<_> = tvs.keys().cloned().collect();
        for old_id in old_ids {
            if !new_ids.contains(&old_id) {
                info!(tv_id = %old_id, "Removing TV (no longer in config)");
                if let Some(tv_arc) = tvs.remove(&old_id) {
                    let mut tv = tv_arc.lock().await;
                    if let Some(mut conn) = tv.connection.take() {
                        let _ = conn.close().await;
                    }
                }
            }
        }

        // Add or update TVs from new config
        for tv_config in new_config.tv {
            if let Some(tv_arc) = tvs.get(&tv_config.id) {
                let mut existing = tv_arc.lock().await;
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
                tvs.insert(
                    tv_config.id.clone(),
                    Arc::new(Mutex::new(ManagedTv::new(tv_config))),
                );
            }
        }

        info!(tv_count = tvs.len(), "Configuration reloaded");
        Ok(())
    }
}
