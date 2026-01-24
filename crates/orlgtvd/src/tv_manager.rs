//! TV connection manager.
//!
//! Manages WebSocket connections to multiple TVs with automatic reconnection.

use orlgtv_core::{Config, TvConfig, TvState, WebOsClient};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Managed TV connection state.
struct ManagedTv {
    /// TV configuration.
    config: TvConfig,
    /// Active WebSocket client (None if disconnected).
    client: Option<WebOsClient>,
    /// Last known TV state.
    state: TvState,
    /// Last error message.
    last_error: Option<String>,
}

impl ManagedTv {
    fn new(config: TvConfig) -> Self {
        Self {
            config,
            client: None,
            state: TvState::default(),
            last_error: None,
        }
    }

    fn is_connected(&self) -> bool {
        self.client.is_some()
    }
}

/// TV connection manager.
///
/// Thread-safe manager for multiple TV connections.
pub struct TvManager {
    /// Global configuration.
    config: Config,
    /// Managed TVs by ID.
    tvs: Arc<RwLock<HashMap<String, ManagedTv>>>,
}

impl TvManager {
    /// Create a new TV manager from configuration.
    pub fn new(config: Config) -> Self {
        let mut tvs = HashMap::new();
        for tv_config in &config.tv {
            tvs.insert(tv_config.id.clone(), ManagedTv::new(tv_config.clone()));
        }

        Self {
            config,
            tvs: Arc::new(RwLock::new(tvs)),
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
            if let Err(e) = orlgtv_core::wake_tv(mac) {
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
        let tv = tvs.get_mut(tv_id).ok_or_else(|| format!("TV not found: {}", tv_id))?;

        if tv.client.is_some() {
            return Ok(());
        }

        info!(tv_id = %tv_id, ip = %tv.config.ip, "Connecting to TV");

        match WebOsClient::connect(&tv.config.ip, tv.config.client_key.clone()).await {
            Ok(client) => {
                info!(tv_id = %tv_id, "Connected to TV");
                tv.client = Some(client);
                tv.last_error = None;
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

    /// Execute a command on a TV.
    ///
    /// Connects if not already connected, then executes the command.
    pub async fn with_client<F, T>(&self, tv_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut WebOsClient) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, orlgtv_core::Error>> + Send + '_>>,
    {
        // First try to connect if needed
        self.connect(tv_id).await?;

        // Then execute the command
        let mut tvs = self.tvs.write().await;
        let tv = tvs.get_mut(tv_id).ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let client = tv.client.as_mut().ok_or("Not connected")?;

        match f(client).await {
            Ok(result) => Ok(result),
            Err(e) => {
                let msg = format!("Command failed: {}", e);
                error!(tv_id = %tv_id, error = %e, "Command failed");
                // Mark as disconnected on error
                tv.client = None;
                tv.last_error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Power on a TV using Wake-on-LAN.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), String> {
        let config = self.get_tv_config(tv_id).await
            .ok_or_else(|| format!("TV not found: {}", tv_id))?;

        let mac = &config.mac;

        info!(tv_id = %tv_id, mac = %mac, "Sending WoL packet");
        orlgtv_core::wake_tv(mac).map_err(|e| format!("WoL failed: {}", e))?;

        // Wait for TV to wake
        let delay = Duration::from_millis(self.config.daemon.wake_delay_ms);
        tokio::time::sleep(delay).await;

        // Try to connect
        self.connect(tv_id).await?;

        // Switch to default input if configured
        if let Some(default_input) = &config.default_input {
            info!(tv_id = %tv_id, input = %default_input, "Switching to default input");
            let input = default_input.clone();
            let _ = self.with_client(tv_id, |client| {
                Box::pin(async move { client.switch_input(&input).await })
            }).await;
        }

        Ok(())
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.power_off().await })
        }).await
    }

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.screen_on().await })
        }).await
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.screen_off().await })
        }).await
    }

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), String> {
        let input = input_id.to_string();
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.switch_input(&input).await })
        }).await
    }

    /// Get available inputs.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move {
                let inputs = client.get_inputs().await?;
                Ok(inputs.into_iter().map(|i| (i.id, i.label)).collect())
            })
        }).await
    }

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u32) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.set_volume(volume as i32).await })
        }).await
    }

    /// Get volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u32, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move {
                let info = client.get_volume().await?;
                Ok(info.volume as u32)
            })
        }).await
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.set_mute(mute).await })
        }).await
    }

    /// Set backlight.
    pub async fn set_backlight(&self, tv_id: &str, backlight: u32) -> Result<(), String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.set_backlight(backlight as i32).await })
        }).await
    }

    /// Get backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u32, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move {
                let backlight = client.get_backlight().await?;
                Ok(backlight as u32)
            })
        }).await
    }

    /// Get TV state.
    pub async fn get_state(&self, tv_id: &str) -> Result<TvState, String> {
        self.with_client(tv_id, |client| {
            Box::pin(async move { client.get_state().await })
        }).await
    }
}
