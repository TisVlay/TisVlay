//! TV connection manager.
//!
//! Manages WebSocket connections to multiple TVs with automatic reconnection.

use tisvlay_core::{Config, TvConfig, TvState};
use tisvlay_lg::WebOsClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
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
    /// Active WebSocket client (None if disconnected).
    client: Option<WebOsClient>,
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
            client: None,
            state: TvState::default(),
            last_error: None,
            reconnect_state: ReconnectState::Idle,
        }
    }

    fn is_connected(&self) -> bool {
        self.client.is_some()
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
        let worker_state_tx = state_change_tx.clone();
        let max_delay = config.daemon.reconnect_interval_ms;
        let worker_handle = tokio::spawn(async move {
            reconnect_worker(worker_tvs, reconnect_rx, max_delay, worker_state_tx).await;
        });

        let manager = Self {
            config,
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
            info!(tv_id = %id, mac = %mac, "Sending WoL packet");
            if let Err(e) = tisvlay_lg::wake_tv(mac) {
                warn!(tv_id = %id, error = %e, "Failed to send WoL");
            }
        }

        // Wait for TVs to wake up
        let delay = Duration::from_millis(self.config.daemon.wake_delay_ms);
        info!("Waiting {:?} for TVs to wake", delay);
        tokio::time::sleep(delay).await;
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

    /// Get TV configuration by ID.
    pub async fn get_tv_config(&self, tv_id: &str) -> Option<TvConfig> {
        let tvs = self.tvs.read().await;
        tvs.get(tv_id).map(|tv| tv.config.clone())
    }

    /// Connect to a TV, returning the client for use.
    async fn connect(&self, tv_id: &str) -> Result<(), String> {
        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        if tv.client.is_some() {
            return Ok(());
        }

        info!(tv_id = %tv_id, ip = %tv.config.ip, "Connecting to TV");

        match WebOsClient::connect(&tv.config.ip, tv.config.client_key.clone()).await {
            Ok(client) => {
                info!(tv_id = %tv_id, "Connected to TV");
                tv.client = Some(client);
                tv.last_error = None;
                tv.reconnect_state = ReconnectState::Idle;

                // Emit connection state change
                let event = StateChangeEvent::connection_change(tv_id, true, None);
                drop(tvs); // Release lock before emitting
                self.emit_state_change(event);

                Ok(())
            }
            Err(e) => {
                let msg = format!("Connection failed: {}", e);
                error!(tv_id = %tv_id, error = %e, "Failed to connect");
                tv.last_error = Some(msg.clone());
                Err(msg)
            }
        }
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

    /// Execute a command on a TV.
    ///
    /// Connects if not already connected, then executes the command.
    pub async fn with_client<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(
            &mut WebOsClient,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, tisvlay_core::Error>> + Send + '_>,
        >,
    {
        // First try to connect if needed
        self.connect(tv_id).await?;

        // Then execute the command
        let mut tvs = self.tvs.write().await;
        let tv = tvs
            .get_mut(tv_id)
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let client = tv.client.as_mut().ok_or("Not connected")?;

        match f(client).await {
            Ok(result) => Ok(result),
            Err(e) => {
                let msg = format!("Command failed: {}", e);
                error!(tv_id = %tv_id, error = %e, "Command failed");
                // Mark as disconnected on error
                tv.client = None;
                tv.last_error = Some(msg.clone());

                // Emit disconnection state change
                let event = StateChangeEvent::connection_change(tv_id, false, Some(&msg));

                // Trigger reconnection (drop lock first)
                let tv_id_owned = tv_id.to_string();
                drop(tvs);

                self.emit_state_change(event);
                self.trigger_reconnect(&tv_id_owned).await;

                Err(msg)
            }
        }
    }

    /// Power on a TV using Wake-on-LAN.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), String> {
        let config = self
            .get_tv_config(tv_id)
            .await
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let mac = &config.mac;

        info!(tv_id = %tv_id, mac = %mac, "Sending WoL packet");
        tisvlay_lg::wake_tv(mac).map_err(|e| format!("WoL failed: {}", e))?;

        // Wait for TV to wake
        let delay = Duration::from_millis(self.config.daemon.wake_delay_ms);
        tokio::time::sleep(delay).await;

        // Try to connect
        self.connect(tv_id).await?;

        // Switch to default input if configured
        if let Some(default_input) = &config.default_input {
            info!(tv_id = %tv_id, input = %default_input, "Switching to default input");
            let input = default_input.clone();
            let _ = self
                .with_client(tv_id, |client| {
                    Box::pin(async move { client.switch_input(&input).await })
                })
                .await;
        }

        Ok(())
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.power_off().await })
        })
        .await
    }

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.screen_on().await })
        })
        .await
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.screen_off().await })
        })
        .await
    }

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), String> {
        let input = input_id.to_string();
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.switch_input(&input).await })
        })
        .await
    }

    /// Get available inputs.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move {
                let inputs = client.get_inputs().await?;
                Ok(inputs.into_iter().map(|i| (i.id, i.label)).collect())
            })
        })
        .await
    }

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.set_volume(volume as i32).await })
        })
        .await
    }

    /// Get volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u32, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move {
                let info = client.get_volume().await?;
                Ok(info.volume as u32)
            })
        })
        .await
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.set_mute(mute).await })
        })
        .await
    }

    /// Set backlight.
    pub async fn set_backlight(&self, tv_id: &str, backlight: u32) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.set_backlight(backlight as i32).await })
        })
        .await
    }

    /// Get backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u32, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move {
                let backlight = client.get_backlight().await?;
                Ok(backlight as u32)
            })
        })
        .await
    }

    /// Get TV state.
    pub async fn get_state(&self, tv_id: &str) -> Result<TvState, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.get_state().await })
        })
        .await
    }

    /// Pair a new TV.
    ///
    /// Connects to the TV at the given IP, triggering a pairing prompt on the TV.
    /// User must accept the prompt for pairing to succeed.
    ///
    /// Returns the TV ID on success.
    pub async fn pair_tv(
        &self,
        ip: &str,
        name: &str,
        mac: Option<&str>,
        id: Option<&str>,
    ) -> Result<String, String> {
        info!(ip = %ip, name = %name, "Pairing TV");

        // Connect to TV (this triggers pairing prompt)
        let client = WebOsClient::connect(ip, None)
            .await
            .map_err(|e| format!("Failed to connect to TV: {}", e))?;

        let client_key = client
            .client_key()
            .ok_or_else(|| "Failed to get client key after pairing".to_string())?
            .to_string();

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
            vendor: "lg".to_string(),
            profile: "default".to_string(),
            hdmi_input: None,
            only_when_active: false,
            client_key: Some(client_key.clone()),
            default_input: None,
            lg: Some(tisvlay_core::LgVendorConfig {
                client_key: Some(client_key),
                default_input: None,
            }),
        };

        // Add to managed TVs
        {
            let mut tvs = self.tvs.write().await;
            tvs.insert(tv_id.clone(), ManagedTv::new(tv_config.clone()));
        }

        // Save config
        self.save_tv_config(&tv_config)?;

        info!(tv_id = %tv_id, "TV paired successfully");
        Ok(tv_id)
    }

    /// Remove a TV.
    ///
    /// Disconnects from the TV and removes it from configuration.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<(), String> {
        info!(tv_id = %tv_id, "Removing TV");

        // Remove from managed TVs
        let removed = {
            let mut tvs = self.tvs.write().await;
            tvs.remove(tv_id).is_some()
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
    ///
    /// Adds new TVs, updates existing ones, and removes TVs that are no longer in config.
    /// Active connections are preserved if the TV config hasn't changed.
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
                tvs.remove(&old_id);
            }
        }

        // Add or update TVs from new config
        for tv_config in new_config.tv {
            if let Some(existing) = tvs.get_mut(&tv_config.id) {
                // Update config if changed (preserve connection if possible)
                if existing.config.ip != tv_config.ip
                    || existing.config.client_key != tv_config.client_key
                {
                    info!(tv_id = %tv_config.id, "Updating TV config (will reconnect)");
                    existing.client = None;
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
}

/// Background worker for handling reconnection attempts with exponential backoff.
async fn reconnect_worker(
    tvs: Arc<RwLock<HashMap<String, ManagedTv>>>,
    mut rx: mpsc::Receiver<ManagerMsg>,
    max_delay_ms: u64,
    state_change_tx: mpsc::UnboundedSender<StateChangeEvent>,
) {
    info!("Reconnection worker started");

    while let Some(msg) = rx.recv().await {
        match msg {
            ManagerMsg::Reconnect(tv_id) => {
                reconnect_tv(&tvs, &tv_id, max_delay_ms, &state_change_tx).await;
            }
        }
    }

    info!("Reconnection worker stopped");
}

/// Attempt to reconnect a TV with exponential backoff.
async fn reconnect_tv(
    tvs: &Arc<RwLock<HashMap<String, ManagedTv>>>,
    tv_id: &str,
    max_delay_ms: u64,
    state_change_tx: &mpsc::UnboundedSender<StateChangeEvent>,
) {
    let mut current_delay_ms = MIN_RECONNECT_DELAY_MS;

    loop {
        // Get current reconnection state
        let (should_reconnect, ip, client_key) = {
            let tvs = tvs.read().await;
            match tvs.get(tv_id) {
                Some(tv) => {
                    // Stop if already connected or reconnection was cancelled
                    if tv.is_connected() || tv.reconnect_state == ReconnectState::Idle {
                        debug!(tv_id = %tv_id, "Reconnection no longer needed");
                        return;
                    }
                    (true, tv.config.ip.clone(), tv.config.client_key.clone())
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
        match WebOsClient::connect(&ip, client_key).await {
            Ok(client) => {
                info!(tv_id = %tv_id, "Reconnection successful");
                {
                    let mut tvs = tvs.write().await;
                    if let Some(tv) = tvs.get_mut(tv_id) {
                        tv.client = Some(client);
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
