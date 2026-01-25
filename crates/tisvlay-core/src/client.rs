//! High-level WebOS TV client.

use crate::connection::Connection;
use crate::endpoints::{audio, input, luna, notification, picture, power, timeouts};
use crate::error::{Error, Result};
use crate::protocol::{
    ExternalInputList, RegisterPayload, RegisteredPayload, Request, Response, ResponseType,
    VolumeInfo, VolumeResponse,
};
use crate::state::{InputInfo, PowerState, TvState};
use crate::wol::{self, MacAddress};
use serde_json::{json, Value};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Client for controlling an LG WebOS TV.
pub struct WebOsClient {
    /// WebSocket connection.
    conn: Connection,
    /// Client key for authentication.
    client_key: Option<String>,
    /// Request ID counter.
    request_id: AtomicU64,
    /// Request timeout.
    timeout: Duration,
}

impl WebOsClient {
    /// Connect to a TV and register.
    ///
    /// If `client_key` is provided, attempts to reconnect with existing pairing.
    /// If `None` or the key is invalid, a pairing prompt will be shown on the TV.
    pub async fn connect(host: &str, client_key: Option<String>) -> Result<Self> {
        let connect_timeout = Duration::from_millis(timeouts::CONNECT_MS);
        let conn = Connection::connect(host, connect_timeout).await?;

        let mut client = Self {
            conn,
            client_key: client_key.clone(),
            request_id: AtomicU64::new(1),
            timeout: Duration::from_millis(timeouts::REQUEST_MS),
        };

        // Register with the TV
        let new_key = client.register(client_key).await?;
        client.client_key = Some(new_key);

        Ok(client)
    }

    /// Register with the TV and get a client key.
    async fn register(&mut self, existing_key: Option<String>) -> Result<String> {
        let payload = RegisterPayload::new(existing_key);
        let request = Request::register(self.next_id(), payload);

        // Use longer timeout for pairing (user needs to accept on TV)
        let pairing_timeout = Duration::from_millis(timeouts::PAIRING_MS);
        self.conn.send(&request).await?;

        // Wait for registered response
        let deadline = tokio::time::Instant::now() + pairing_timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::PairingTimeout);
            }

            let response = self.conn.recv_timeout(remaining).await?;

            match response.msg_type {
                ResponseType::Registered => {
                    let payload: RegisteredPayload = serde_json::from_value(response.payload)?;
                    tracing::info!("Registered with TV, got client key");
                    return Ok(payload.client_key);
                }
                ResponseType::Error => {
                    if let Some(error) = response.error {
                        if error.contains("cancelled") || error.contains("rejected") {
                            return Err(Error::PairingRejected);
                        }
                        return Err(Error::TvError(error));
                    }
                    return Err(Error::PairingRejected);
                }
                _ => {
                    // Ignore other messages during registration
                    tracing::trace!(msg_type = ?response.msg_type, "Ignoring message during registration");
                }
            }
        }
    }

    /// Get the client key (for saving to config).
    pub fn client_key(&self) -> Option<&str> {
        self.client_key.as_deref()
    }

    /// Generate the next request ID.
    fn next_id(&self) -> String {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        format!("req_{}", id)
    }

    /// Send a command and get the response.
    async fn command(&mut self, uri: &str, payload: Value) -> Result<Response> {
        let request = Request::command(self.next_id(), uri, payload);
        let response = self.conn.request(&request, self.timeout).await?;

        if response.msg_type == ResponseType::Error {
            if let Some(error) = response.error {
                return Err(Error::TvError(error));
            }
        }

        Ok(response)
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Turn off the TV (standby).
    pub async fn power_off(&mut self) -> Result<()> {
        self.command(power::TURN_OFF, json!({})).await?;
        Ok(())
    }

    /// Turn off the screen (OLED-safe blank, TV stays on).
    pub async fn screen_off(&mut self) -> Result<()> {
        self.command(power::TURN_OFF_SCREEN, json!({})).await?;
        Ok(())
    }

    /// Turn on the screen (resume from blank).
    pub async fn screen_on(&mut self) -> Result<()> {
        self.command(power::TURN_ON_SCREEN, json!({})).await?;
        Ok(())
    }

    /// Get the current power state.
    pub async fn get_power_state(&mut self) -> Result<PowerState> {
        let response = self.command(power::GET_POWER_STATE, json!({})).await?;

        let state_str = response
            .payload
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Ok(PowerState::from_str(state_str))
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch to a specific input.
    pub async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        self.command(input::SWITCH_INPUT, json!({"inputId": input_id}))
            .await?;
        Ok(())
    }

    /// Get list of available inputs.
    pub async fn get_inputs(&mut self) -> Result<Vec<InputInfo>> {
        let response = self
            .command(input::GET_EXTERNAL_INPUT_LIST, json!({}))
            .await?;

        let input_list: ExternalInputList = serde_json::from_value(response.payload)?;

        Ok(input_list
            .devices
            .into_iter()
            .map(|d| InputInfo {
                id: d.id,
                label: d.label,
                connected: d.connected,
            })
            .collect())
    }

    // -------------------------------------------------------------------------
    // Audio Control
    // -------------------------------------------------------------------------

    /// Set the volume (0-100).
    pub async fn set_volume(&mut self, volume: i32) -> Result<()> {
        let volume = volume.clamp(0, 100);
        self.command(audio::SET_VOLUME, json!({"volume": volume}))
            .await?;
        Ok(())
    }

    /// Get the current volume.
    pub async fn get_volume(&mut self) -> Result<VolumeInfo> {
        let response = self.command(audio::GET_VOLUME, json!({})).await?;
        let vol_response: VolumeResponse = serde_json::from_value(response.payload)?;
        vol_response
            .into_volume_info()
            .ok_or_else(|| Error::InvalidResponse("No volume in response".to_string()))
    }

    /// Set mute state.
    pub async fn set_mute(&mut self, mute: bool) -> Result<()> {
        self.command(audio::SET_MUTE, json!({"mute": mute})).await?;
        Ok(())
    }

    /// Get current mute state.
    pub async fn get_mute(&mut self) -> Result<bool> {
        let response = self.command(audio::GET_MUTE, json!({})).await?;
        let muted = response
            .payload
            .get("mute")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(muted)
    }

    /// Increase volume by one step.
    pub async fn volume_up(&mut self) -> Result<()> {
        self.command(audio::VOLUME_UP, json!({})).await?;
        Ok(())
    }

    /// Decrease volume by one step.
    pub async fn volume_down(&mut self) -> Result<()> {
        self.command(audio::VOLUME_DOWN, json!({})).await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Picture Control
    // -------------------------------------------------------------------------

    /// Set backlight (OLED pixel brightness, 0-100).
    ///
    /// Uses the Luna service workaround via createAlert to bypass permission
    /// restrictions on the settings API. The alert is automatically closed
    /// to avoid showing a prompt on the TV.
    pub async fn set_backlight(&mut self, backlight: i32) -> Result<()> {
        let backlight = backlight.clamp(0, 100);

        // Build the Luna service parameters
        let luna_params = json!({
            "category": "picture",
            "settings": {
                "backlight": backlight
            }
        });

        // Wrap the Luna call inside an alert payload
        // This workaround is used by LGTVCompanion to bypass permission restrictions
        let alert_payload = json!({
            "message": " ",
            "buttons": [{
                "label": "",
                "onClick": luna::SET_SYSTEM_SETTINGS,
                "params": luna_params
            }],
            "onclose": {
                "uri": luna::SET_SYSTEM_SETTINGS,
                "params": luna_params
            },
            "onfail": {
                "uri": luna::SET_SYSTEM_SETTINGS,
                "params": luna_params
            }
        });

        let response = self
            .command(notification::CREATE_ALERT, alert_payload)
            .await?;

        // Extract alertId and close the alert to avoid showing a prompt on the TV
        if let Some(alert_id) = response.payload.get("alertId").and_then(|v| v.as_str()) {
            tracing::debug!(alert_id, "Closing Luna alert");
            let _ = self
                .command(notification::CLOSE_ALERT, json!({"alertId": alert_id}))
                .await;
        }

        Ok(())
    }

    /// Get current backlight (OLED pixel brightness).
    pub async fn get_backlight(&mut self) -> Result<i32> {
        let response = self
            .command(
                picture::GET_SYSTEM_SETTINGS,
                json!({
                    "category": "picture",
                    "keys": ["backlight"]
                }),
            )
            .await?;

        let backlight = response
            .payload
            .get("settings")
            .and_then(|s| s.get("backlight"))
            .and_then(|v| v.as_i64())
            .unwrap_or(50) as i32;

        Ok(backlight)
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get the current TV state.
    pub async fn get_state(&mut self) -> Result<TvState> {
        let mut state = TvState::new();

        // Get power state
        if let Ok(power_state) = self.get_power_state().await {
            state.update_power(power_state);
        }

        // Get volume
        if let Ok(volume_info) = self.get_volume().await {
            state.volume = Some(volume_info.volume);
            state.muted = Some(volume_info.muted);
        }

        // Get inputs
        if let Ok(inputs) = self.get_inputs().await {
            state.inputs = inputs;
        }

        // Get backlight
        if let Ok(backlight) = self.get_backlight().await {
            state.backlight = Some(backlight);
        }

        Ok(state)
    }

    /// Close the connection.
    pub async fn close(self) -> Result<()> {
        self.conn.close().await
    }
}

// -----------------------------------------------------------------------------
// Wake-on-LAN helper functions
// -----------------------------------------------------------------------------

/// Wake a TV using Wake-on-LAN.
///
/// This only sends the WoL packet. You'll need to wait for the TV to boot
/// before connecting.
pub fn wake_tv(mac: &str) -> Result<()> {
    let mac = MacAddress::parse(mac)?;
    wol::wake(&mac)
}

/// Wake a TV and wait for it to be ready.
pub async fn wake_and_wait(mac: &str, ip: Option<&str>, delay: Duration) -> Result<()> {
    let mac_addr = MacAddress::parse(mac)?;
    let ip_addr = ip.and_then(|s| s.parse::<Ipv4Addr>().ok());

    wol::wake_reliable(&mac_addr, ip_addr)?;

    tracing::info!(delay_ms = delay.as_millis(), "Waiting for TV to wake up");
    tokio::time::sleep(delay).await;

    Ok(())
}

/// Wake a TV, wait, then connect.
pub async fn wake_and_connect(
    mac: &str,
    ip: &str,
    client_key: Option<String>,
    wake_delay: Duration,
) -> Result<WebOsClient> {
    wake_and_wait(mac, Some(ip), wake_delay).await?;
    WebOsClient::connect(ip, client_key).await
}
