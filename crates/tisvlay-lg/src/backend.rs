//! LG WebOS backend implementation.
//!
//! This module provides the trait implementations for LG WebOS TVs.

use crate::client::WebOsClient;
use crate::config::LgVendorConfig;
use async_trait::async_trait;
use std::any::Any;
use tisvlay_core::{
    Capabilities, Credentials, Error, InputInfo, MacAddress, PairingCommand, PairingSession,
    PairingState, PowerState, Result, TvBackend, TvConnection, TvConnector, TvState,
};
#[cfg(feature = "discovery")]
use tisvlay_core::{DiscoveredTv, TvDiscovery};
use tracing::{debug, info, warn};

/// Helper to deserialize LgVendorConfig from a toml::Value.
fn parse_config(vendor_config: &toml::Value) -> Result<LgVendorConfig> {
    vendor_config
        .clone()
        .try_into()
        .map_err(|e| Error::backend("lg", format!("Invalid vendor config: {}", e)))
}

/// LG WebOS backend.
///
/// This provides metadata about the LG WebOS backend and its capabilities.
#[derive(Debug, Clone, Default)]
pub struct LgBackend;

impl LgBackend {
    /// Create a new LG backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl TvBackend for LgBackend {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn vendor_name(&self) -> &'static str {
        crate::VENDOR_NAME
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }
}

/// LG WebOS TV connection.
///
/// Wraps a `WebOsClient` and implements the tisvlay-core connection traits.
/// Handles auto-reconnection transparently when the WebSocket connection drops.
pub struct LgConnection {
    client: Option<WebOsClient>,
    config: LgVendorConfig,
    capabilities: Capabilities,
}

impl LgConnection {
    /// Create a new LG connection from a WebOsClient and its config.
    pub fn new(client: WebOsClient, config: LgVendorConfig) -> Self {
        Self {
            client: Some(client),
            config,
            capabilities: Capabilities::ALL,
        }
    }

    /// Ensure the WebSocket connection is alive, reconnecting if stale.
    async fn ensure_connected(&mut self) -> Result<&mut WebOsClient> {
        let needs_reconnect = match &self.client {
            Some(client) => !client.is_connected(),
            None => true,
        };

        if needs_reconnect {
            info!(ip = %self.config.ip, "Reconnecting to LG TV (stale connection)");
            let client =
                WebOsClient::connect(&self.config.ip, self.config.client_key.clone()).await?;
            self.client = Some(client);
        }

        self.client.as_mut().ok_or(Error::ConnectionClosed)
    }
}

#[async_trait]
impl TvConnection for LgConnection {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn is_connected(&self) -> bool {
        self.client.as_ref().is_some_and(|c| c.is_connected())
    }

    async fn get_state(&mut self) -> Result<TvState> {
        self.ensure_connected().await?.get_state().await
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(ref mut client) = self.client {
            client.close().await?;
        }
        self.client = None;
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    // =========================================================================
    // Feature methods (overriding TvConnection defaults)
    // =========================================================================

    async fn power_off(&mut self) -> Result<()> {
        if let Some(ref mut client) = self.client {
            let _ = client.power_off().await;
            let _ = client.close().await;
        }
        self.client = None;
        Ok(())
    }

    async fn get_power_state(&mut self) -> Result<PowerState> {
        self.ensure_connected().await?.get_power_state().await
    }

    async fn screen_on(&mut self) -> Result<()> {
        self.ensure_connected().await?.screen_on().await
    }

    async fn screen_off(&mut self) -> Result<()> {
        self.ensure_connected().await?.screen_off().await
    }

    async fn is_screen_on(&mut self) -> Result<bool> {
        let state = self.ensure_connected().await?.get_power_state().await?;
        Ok(!matches!(state, PowerState::ScreenOff))
    }

    async fn set_volume(&mut self, level: u8) -> Result<()> {
        self.ensure_connected()
            .await?
            .set_volume(level as i32)
            .await
    }

    async fn get_volume(&mut self) -> Result<u8> {
        let info = self.ensure_connected().await?.get_volume().await?;
        Ok(info.volume as u8)
    }

    async fn set_mute(&mut self, mute: bool) -> Result<()> {
        self.ensure_connected().await?.set_mute(mute).await
    }

    async fn is_muted(&mut self) -> Result<bool> {
        let info = self.ensure_connected().await?.get_volume().await?;
        Ok(info.muted)
    }

    async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        self.ensure_connected().await?.switch_input(input_id).await
    }

    async fn get_current_input(&mut self) -> Result<Option<String>> {
        let state = self.ensure_connected().await?.get_state().await?;
        Ok(state.current_input)
    }

    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        self.ensure_connected().await?.get_inputs().await
    }

    async fn set_backlight(&mut self, level: u8) -> Result<()> {
        self.ensure_connected()
            .await?
            .set_backlight(level as i32)
            .await
    }

    async fn get_backlight(&mut self) -> Result<u8> {
        let level = self.ensure_connected().await?.get_backlight().await?;
        Ok(level as u8)
    }
}

/// Factory for creating LG WebOS connections.
pub struct LgConnector;

impl LgConnector {
    /// Create a new LG connector.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LgConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TvConnector for LgConnector {
    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = parse_config(vendor_config)?;

        debug!(ip = %config.ip, has_key = config.client_key.is_some(), "Connecting to LG TV");

        let client = WebOsClient::connect(&config.ip, config.client_key.clone()).await?;
        Ok(Box::new(LgConnection::new(client, config)))
    }

    fn display_address(&self, vendor_config: &toml::Value) -> String {
        parse_config(vendor_config)
            .map(|c| c.ip)
            .unwrap_or_else(|_| "unknown".to_string())
    }

    fn supports_wake(&self, vendor_config: &toml::Value) -> bool {
        parse_config(vendor_config)
            .map(|c| c.mac.is_some())
            .unwrap_or(false)
    }

    async fn wake(&self, vendor_config: &toml::Value) -> Result<()> {
        let config = parse_config(vendor_config)?;
        if let Some(ref mac) = config.mac {
            let mac_addr = MacAddress::parse(mac)?;

            // Try sending WoL, retrying if network is not yet up (e.g., after system resume).
            // The first attempt may fail with "Network is unreachable" while the
            // desktop's network interface is still recovering from sleep.
            let mut last_err = None;
            for attempt in 1..=8u32 {
                match tisvlay_core::wol::wake(&mac_addr) {
                    Ok(()) => {
                        debug!(ip = %config.ip, mac = %mac, attempt, "WoL packet sent");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!(mac = %mac, attempt, error = %e, "WoL send failed, retrying");
                        last_err = Some(e);
                        let delay = if attempt <= 2 { 2000 } else { 3000 };
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
            }

            Err(last_err.unwrap_or_else(|| Error::WakeOnLan("WoL failed after retries".into())))
        } else {
            Ok(())
        }
    }

    async fn wake_and_connect(
        &self,
        vendor_config: &toml::Value,
        boot_delay: std::time::Duration,
        max_attempts: u32,
        retry_interval: std::time::Duration,
    ) -> Result<Box<dyn TvConnection>> {
        self.wake(vendor_config).await?;
        tokio::time::sleep(boot_delay).await;

        let config = parse_config(vendor_config)?;
        let mut last_err = None;

        for attempt in 1..=max_attempts {
            // Resend WoL before retrying — earlier packets may have been
            // lost if the desktop network wasn't up after system resume.
            if attempt > 1 {
                if let Some(ref mac) = config.mac {
                    if let Ok(mac_addr) = MacAddress::parse(mac) {
                        if let Err(e) = tisvlay_core::wol::wake(&mac_addr) {
                            warn!(mac = %mac, attempt, error = %e, "WoL resend failed");
                        }
                    }
                }
            }

            match self.connect(vendor_config).await {
                Ok(conn) => {
                    info!(ip = %config.ip, attempt, "LG TV online after wake");
                    return Ok(conn);
                }
                Err(e) => {
                    debug!(ip = %config.ip, attempt, error = %e, "LG TV not ready yet");
                    last_err = Some(e);
                    tokio::time::sleep(retry_interval).await;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| Error::Backend {
            vendor: "lg".into(),
            message: format!("LG TV at {} did not come online after wake", config.ip),
            source: None,
        }))
    }

    fn start_pairing(&self, vendor_config: &toml::Value) -> Result<PairingSession> {
        let config = parse_config(vendor_config)?;
        let ip = config.ip.clone();

        let (session, mut command_rx, state_tx) = PairingSession::create();

        // Spawn the pairing task
        tokio::spawn(async move {
            // Update state to awaiting TV prompt
            let _ = state_tx.send(PairingState::awaiting_tv_prompt(
                "Please accept the pairing prompt on your LG TV",
            ));

            // Connect and wait for pairing
            match WebOsClient::connect(&ip, None).await {
                Ok(client) => {
                    if let Some(key) = client.client_key() {
                        info!(ip = %ip, "LG TV pairing successful");
                        let _ = state_tx.send(PairingState::complete(Credentials::with_type(
                            key,
                            "client_key",
                        )));
                    } else {
                        warn!(ip = %ip, "LG TV connected but no client key received");
                        let _ = state_tx.send(PairingState::failed(
                            tisvlay_core::PairingError::connection_failed(
                                "Connected but no client key received",
                            ),
                        ));
                    }
                }
                Err(e) => {
                    warn!(ip = %ip, error = %e, "LG TV pairing failed");
                    let state = match &e {
                        Error::PairingRejected => {
                            PairingState::failed(tisvlay_core::PairingError::rejected())
                        }
                        Error::PairingTimeout => {
                            PairingState::failed(tisvlay_core::PairingError::timeout())
                        }
                        _ => PairingState::failed(tisvlay_core::PairingError::connection_failed(
                            e.to_string(),
                        )),
                    };
                    let _ = state_tx.send(state);
                }
            }

            // Handle cancel command if received
            while let Some(cmd) = command_rx.recv().await {
                match cmd {
                    PairingCommand::Cancel => {
                        let _ = state_tx
                            .send(PairingState::failed(tisvlay_core::PairingError::cancelled()));
                        break;
                    }
                    PairingCommand::SubmitInput(_) => {
                        // LG doesn't use PIN/password input
                    }
                }
            }
        });

        Ok(session)
    }

    fn build_config_from_pairing(
        &self,
        pairing_params: &toml::Value,
        credentials: &Credentials,
    ) -> Result<toml::Value> {
        let mut config = parse_config(pairing_params)?;
        config.client_key = Some(credentials.data.clone());
        toml::Value::try_from(&config)
            .map_err(|e| Error::backend("lg", format!("Failed to serialize config: {}", e)))
    }
}

/// LG-specific network discovery.
#[cfg(feature = "discovery")]
pub struct LgDiscovery;

#[cfg(feature = "discovery")]
impl LgDiscovery {
    /// Create a new LG discovery instance.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "discovery")]
impl Default for LgDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "discovery")]
#[async_trait]
impl TvDiscovery for LgDiscovery {
    async fn discover(&self, timeout: std::time::Duration) -> Result<Vec<DiscoveredTv>> {
        crate::discovery::discover_lg_tvs(timeout).await
    }
}
