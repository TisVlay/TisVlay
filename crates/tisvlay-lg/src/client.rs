//! High-level WebOS TV client.

use crate::connection::Connection;
use crate::endpoints::{audio, input, luna, notification, picture, power, timeouts};
use crate::protocol::{
    ExternalInputList, RegisterPayload, RegisteredPayload, Request, Response, ResponseType,
    VolumeInfo, VolumeResponse,
};
use serde_json::{json, Value};
use std::time::Duration;
use tisvlay_core::{Error, InputInfo, PowerState, Result, TvState};
use tokio::sync::mpsc;

/// Client for controlling an LG WebOS TV.
pub struct WebOsClient {
    /// WebSocket connection (None if closed).
    conn: Option<Connection>,
    /// Client key for authentication.
    client_key: Option<String>,
    /// Request ID counter.
    request_id: u64,
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
            conn: Some(conn),
            client_key: client_key.clone(),
            request_id: 1,
            timeout: Duration::from_millis(timeouts::REQUEST_MS),
        };

        // Register with the TV
        let new_key = client.register(client_key).await?;
        client.client_key = Some(new_key);

        Ok(client)
    }

    /// Check if the connection is still active.
    pub fn is_connected(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| c.is_alive())
    }

    /// Get a reference to the connection, returning an error if closed.
    fn conn(&self) -> Result<&Connection> {
        self.conn.as_ref().ok_or(Error::ConnectionClosed)
    }

    /// Get a mutable reference to the connection, returning an error if closed.
    fn conn_mut(&mut self) -> Result<&mut Connection> {
        self.conn.as_mut().ok_or(Error::ConnectionClosed)
    }

    /// Register with the TV and get a client key.
    ///
    /// Uses the multiplexed `request()` method which routes the matching
    /// response via a oneshot channel. Intermediate messages (e.g.,
    /// subscription updates) are handled by the background reader and
    /// do not interfere with registration.
    async fn register(&mut self, existing_key: Option<String>) -> Result<String> {
        let payload = RegisterPayload::new(existing_key);
        let request = Request::register(self.next_id(), payload);

        // Use longer timeout for pairing (user needs to accept on TV)
        let pairing_timeout = Duration::from_millis(timeouts::PAIRING_MS);
        let response = self.conn()?.request(&request, pairing_timeout).await?;

        match response.msg_type {
            ResponseType::Registered => {
                let payload: RegisteredPayload = serde_json::from_value(response.payload)?;
                tracing::info!("Registered with TV, got client key");
                Ok(payload.client_key)
            }
            ResponseType::Error => {
                if let Some(error) = response.error {
                    if error.contains("cancelled") || error.contains("rejected") {
                        return Err(Error::PairingRejected);
                    }
                    return Err(Error::TvError(error));
                }
                Err(Error::PairingRejected)
            }
            _ => Err(Error::InvalidResponse(format!(
                "Unexpected response type during registration: {:?}",
                response.msg_type
            ))),
        }
    }

    /// Get the client key (for saving to config).
    pub fn client_key(&self) -> Option<&str> {
        self.client_key.as_deref()
    }

    /// Generate the next request ID.
    fn next_id(&mut self) -> String {
        let id = self.request_id;
        self.request_id += 1;
        format!("req_{}", id)
    }

    /// Send a command and get the response.
    async fn command(&mut self, uri: &str, payload: Value) -> Result<Response> {
        let request = Request::command(self.next_id(), uri, payload);
        let timeout = self.timeout;
        let response = self.conn()?.request(&request, timeout).await?;

        if response.msg_type == ResponseType::Error {
            if let Some(error) = response.error {
                return Err(Error::TvError(error));
            }
        }

        Ok(response)
    }

    // -------------------------------------------------------------------------
    // Subscriptions
    // -------------------------------------------------------------------------

    /// Subscribe to a single SSAP endpoint.
    ///
    /// Sends a subscribe request and returns the initial response (which
    /// typically contains the current value). Subsequent updates arrive on
    /// the subscription channel (see [`take_subscription_rx`]).
    pub async fn subscribe(&mut self, uri: &str) -> Result<Response> {
        let request = Request::subscribe(self.next_id(), uri);
        let timeout = self.timeout;
        let response = self.conn()?.request(&request, timeout).await?;

        if response.msg_type == ResponseType::Error {
            if let Some(error) = &response.error {
                tracing::warn!(uri, error, "Subscribe returned error");
            }
        }

        Ok(response)
    }

    /// Subscribe to all state endpoints used for push-based updates.
    ///
    /// After calling this, subscription updates will arrive on the channel
    /// returned by [`take_subscription_rx`].
    pub async fn subscribe_state_updates(&mut self) -> Result<()> {
        // Each subscribe call returns the initial value; we discard it here
        // since the caller will get ongoing updates via the subscription channel.
        self.subscribe(power::GET_POWER_STATE).await?;
        self.subscribe(audio::GET_VOLUME).await?;
        self.subscribe(audio::GET_MUTE).await?;
        self.subscribe(input::GET_EXTERNAL_INPUT_LIST).await?;
        self.subscribe(input::GET_CURRENT_INPUT).await?;
        Ok(())
    }

    /// Take the subscription receiver from the underlying connection.
    ///
    /// Returns `Some` on the first call, `None` on subsequent calls.
    pub fn take_subscription_rx(&mut self) -> Option<mpsc::UnboundedReceiver<Response>> {
        self.conn_mut().ok()?.take_subscription_rx()
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
    ///
    /// Idempotent: if the screen is already off, the TV returns a 500 error
    /// which is treated as success.
    pub async fn screen_off(&mut self) -> Result<()> {
        match self.command(power::TURN_OFF_SCREEN, json!({})).await {
            Ok(_) => Ok(()),
            Err(Error::TvError(_)) => {
                tracing::debug!(
                    "screen_off returned error (screen likely already off), treating as success"
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Turn on the screen (resume from blank).
    ///
    /// Idempotent: if the screen is already on, the TV returns a 500 error
    /// which is treated as success.
    pub async fn screen_on(&mut self) -> Result<()> {
        match self.command(power::TURN_ON_SCREEN, json!({})).await {
            Ok(_) => Ok(()),
            Err(Error::TvError(_)) => {
                tracing::debug!(
                    "screen_on returned error (screen likely already on), treating as success"
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Get the current power state.
    pub async fn get_power_state(&mut self) -> Result<PowerState> {
        let response = self.command(power::GET_POWER_STATE, json!({})).await?;

        let state_str = response
            .payload
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Ok(PowerState::parse(state_str))
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
    pub async fn set_volume(&mut self, volume: u8) -> Result<()> {
        let volume = volume.min(100) as i32;
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
    /// The SSAP `setSystemSettings` endpoint returns 401 even with
    /// `WRITE_SETTINGS` in the signed manifest. Uses the Luna service
    /// workaround via createAlert instead (shared by bscpylgtv,
    /// LGTVCompanion, etc.).
    pub async fn set_backlight(&mut self, backlight: u8) -> Result<()> {
        let backlight = backlight.min(100) as i32;

        let luna_params = json!({
            "category": "picture",
            "settings": {
                "backlight": backlight
            }
        });

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

        // Close the alert to avoid showing a prompt on the TV
        if let Some(alert_id) = response.payload.get("alertId").and_then(|v| v.as_str()) {
            tracing::debug!(alert_id, "Closing Luna alert");
            let _ = self
                .command(notification::CLOSE_ALERT, json!({"alertId": alert_id}))
                .await;
        }

        Ok(())
    }

    /// Get current backlight (OLED pixel brightness, 0-100).
    pub async fn get_backlight(&mut self) -> Result<u8> {
        let response = self
            .command(
                picture::GET_SYSTEM_SETTINGS,
                json!({
                    "category": "picture",
                    "keys": ["backlight"]
                }),
            )
            .await?;

        let raw = response
            .payload
            .get("settings")
            .and_then(|s| s.get("backlight"))
            .and_then(|v| v.as_i64())
            .unwrap_or(50);

        Ok(raw.clamp(0, 100) as u8)
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get the current TV state.
    pub async fn get_state(&mut self) -> Result<TvState> {
        let mut state = TvState::new();

        // Get power state
        match self.get_power_state().await {
            Ok(power_state) => state.update_power(power_state),
            Err(e) => tracing::debug!(error = %e, "get_state: power state failed"),
        }

        // Get volume and mute state separately — the getVolume response
        // doesn't always include the mute field on all LG TV firmware.
        match self.get_volume().await {
            Ok(volume_info) => state.volume = Some(volume_info.volume.clamp(0, 100) as u8),
            Err(e) => tracing::debug!(error = %e, "get_state: volume failed"),
        }
        match self.get_mute().await {
            Ok(muted) => state.muted = Some(muted),
            Err(e) => tracing::debug!(error = %e, "get_state: mute failed"),
        }

        // Get inputs
        match self.get_inputs().await {
            Ok(inputs) => state.inputs = inputs,
            Err(e) => tracing::debug!(error = %e, "get_state: inputs failed"),
        }

        // Get backlight
        match self.get_backlight().await {
            Ok(backlight) => state.backlight = Some(backlight),
            Err(e) => tracing::debug!(error = %e, "get_state: backlight failed"),
        }

        Ok(state)
    }

    /// Close the connection.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut conn) = self.conn.take() {
            conn.close().await
        } else {
            Ok(())
        }
    }
}
