//! High-level WebOS TV client.

use crate::connection::Connection;
use crate::endpoints::{audio, input, luna, notification, picture, power, system, timeouts};
use crate::protocol::{
    ExternalInputList, RegisterPayload, RegisteredPayload, Request, Response, ResponseType,
    VolumeInfo, VolumeResponse,
};
use serde_json::{Value, json};
use std::time::Duration;
use tisvlay_core::{Error, HardwareId, InputInfo, PowerState, Result, TvState};
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

impl std::fmt::Debug for WebOsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebOsClient")
            .field("connected", &self.conn.is_some())
            .field("has_client_key", &self.client_key.is_some())
            .field("request_id", &self.request_id)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Whether an LG error string represents the idempotent "screen
/// already in target state" case (`500 <message>`).
///
/// LG returns errors as `"<code> <message>"`. The `500` family is the
/// "method not allowed in current state" path that fires when
/// `turnOnScreen` / `turnOffScreen` is called while the TV is already
/// in the requested state. Masking it preserves the no-op UX for
/// direct callers (tray, GUI).
///
/// Other codes — notably `-102 not in screen-off substate`, which
/// fires when the TV has slipped past Screen Off into a deeper power
/// state — must NOT be masked: the reliable dispatch tier reads them
/// to decide whether to escalate to `power_on` with WoL.
fn is_idempotent_screen_error(error: &str) -> bool {
    error.split_whitespace().next() == Some("500")
}

impl WebOsClient {
    /// Connect to a TV and register, trying `wss://` first and
    /// falling back to `ws://`. Used during pairing and for legacy
    /// configs that haven't yet recorded which scheme the TV speaks.
    ///
    /// If `client_key` is provided, attempts to reconnect with
    /// existing pairing. If `None` or the key is invalid, a pairing
    /// prompt will be shown on the TV.
    pub async fn connect(host: &str, client_key: Option<String>) -> Result<Self> {
        let connect_timeout = Duration::from_millis(timeouts::CONNECT_MS);
        let conn = Connection::connect(host, connect_timeout).await?;
        Self::register_on(conn, client_key).await
    }

    /// Connect via a known [`WebOsScheme`] and register. Used on
    /// reconnects when the pairing flow has already recorded which
    /// scheme the TV speaks into `LgVendorConfig.scheme` — single
    /// dispatch, no fallback round-trip.
    pub async fn connect_with_scheme(
        host: &str,
        scheme: crate::config::WebOsScheme,
        client_key: Option<String>,
    ) -> Result<Self> {
        use crate::config::WebOsScheme;
        let connect_timeout = Duration::from_millis(timeouts::CONNECT_MS);
        let conn = match scheme {
            WebOsScheme::Wss => Connection::connect_wss(host, connect_timeout).await?,
            WebOsScheme::Ws => Connection::connect_ws(host, connect_timeout).await?,
        };
        Self::register_on(conn, client_key).await
    }

    async fn register_on(conn: Connection, client_key: Option<String>) -> Result<Self> {
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

    /// Connect to a fake SSAP server at the given URL and register.
    ///
    /// Used by tests that run a [`FakeSsapServer`](crate::test_server::FakeSsapServer)
    /// on localhost. Bypasses the wss→ws fallback and connects directly.
    #[cfg(test)]
    pub(crate) async fn connect_url(url: &str, client_key: Option<String>) -> Result<Self> {
        let connect_timeout = Duration::from_millis(timeouts::CONNECT_MS);
        let conn = Connection::connect_url_plain(url, connect_timeout).await?;

        let mut client = Self {
            conn: Some(conn),
            client_key: client_key.clone(),
            request_id: 1,
            timeout: Duration::from_millis(timeouts::REQUEST_MS),
        };

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

        // The TV may send multiple messages for the same request ID:
        //   1. Response { pairingType: "PROMPT", returnValue: true } — prompt shown
        //   2. Registered { client-key: "..." } — user accepted
        //
        // The multiplexed request() uses a oneshot channel: the background
        // reader removes the pending entry after delivering the first match.
        // To handle the two-phase flow, we use request_wait() which only
        // registers the oneshot (no send) for the second phase.
        let pairing_timeout = Duration::from_millis(timeouts::PAIRING_MS);
        let deadline = tokio::time::Instant::now() + pairing_timeout;

        // Phase 1: send the register request and get the first response.
        let response = self.conn()?.request(&request, pairing_timeout).await?;
        tracing::debug!(msg_type = ?response.msg_type, payload = %response.payload, "Registration response (phase 1)");

        match response.msg_type {
            ResponseType::Registered => {
                let payload: RegisteredPayload = serde_json::from_value(response.payload)?;
                tracing::info!("Registered with TV, got client key");
                return Ok(payload.client_key);
            }
            ResponseType::Response => {
                // Intermediate acknowledgment — pairing prompt shown on TV.
                // The user needs to accept; the TV will then send Registered.
                tracing::debug!("Pairing prompt shown on TV, waiting for user to accept");
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
                return Err(Error::InvalidResponse(format!(
                    "Unexpected response type during registration: {:?}",
                    response.msg_type
                )));
            }
        }

        // Phase 2: wait for the Registered response (no re-send).
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let response = self.conn()?.request_wait(&request.id, remaining).await?;
        tracing::debug!(msg_type = ?response.msg_type, payload = %response.payload, "Registration response (phase 2)");

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

    /// WebSocket scheme the underlying connection succeeded over.
    /// Returns `None` when the connection is closed. Read by the
    /// pairing flow so the working scheme can be persisted to
    /// `LgVendorConfig.scheme` and skip the wss→ws fallback on
    /// subsequent reconnects.
    pub(crate) fn scheme(&self) -> Option<crate::config::WebOsScheme> {
        self.conn.as_ref().map(|c| c.scheme())
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
    /// Returns a mapping of request ID → endpoint URI so the caller can
    /// identify which subscription each update belongs to (SSAP subscription
    /// updates carry the request ID but not the URI), plus the raw initial
    /// responses from each successful subscription (which contain the current
    /// values at subscribe time).
    ///
    /// After calling this, subscription updates will arrive on the channel
    /// returned by [`take_subscription_rx`].
    pub async fn subscribe_state_updates(
        &mut self,
    ) -> Result<(
        std::collections::HashMap<String, String>,
        Vec<(String, Response)>,
    )> {
        use futures_util::future::join_all;
        use serde_json::json;

        // Build all the subscribe requests upfront. ID allocation
        // is the only `&mut self` step; the wire send + response
        // wait below run against a shared `&Connection` so the
        // requests can fly in parallel.
        let plain_endpoints: &[&str] = &[
            power::GET_POWER_STATE,
            audio::GET_VOLUME,
            audio::GET_MUTE,
            input::GET_EXTERNAL_INPUT_LIST,
            input::GET_CURRENT_INPUT,
        ];
        let payload_endpoints: Vec<(&str, serde_json::Value)> = vec![(
            picture::GET_SYSTEM_SETTINGS,
            json!({"category": "picture", "keys": ["backlight"]}),
        )];

        // (request_id, uri-or-synthetic-uri-for-bookkeeping, request)
        let mut prepared: Vec<(String, String, Request)> =
            Vec::with_capacity(plain_endpoints.len() + payload_endpoints.len());

        for uri in plain_endpoints {
            let request = Request::subscribe(self.next_id(), *uri);
            prepared.push((request.id.clone(), uri.to_string(), request));
        }
        for (uri, payload) in &payload_endpoints {
            let request = Request::subscribe_with_payload(self.next_id(), *uri, payload.clone());
            // Synthetic URI distinguishes multiple `getSystemSettings`
            // calls at routing time — each (category, keys) pair
            // gets its own pseudo-URI so the state forwarder can
            // tell them apart.
            let synthetic_uri = format!("{}#picture/backlight", uri);
            prepared.push((request.id.clone(), synthetic_uri, request));
        }

        let timeout = self.timeout;
        let conn = self.conn()?;

        // Fire every subscribe concurrently. WebSocket is full-duplex
        // and the response reader correlates by request `id`, so
        // multiple in-flight subscribes are fine. Total wall time is
        // max single-subscribe latency rather than the sum, which
        // compresses subscription bring-up after a power-cycle from
        // ~7 s of serialized round-trips to ~1 s.
        //
        // SSAP has no batch-fetch endpoint that returns
        // power+volume+input+backlight in one call (each lives under
        // its own service URI), so parallelism is the only knob.
        let results = join_all(
            prepared
                .iter()
                .map(|(_, _, req)| conn.request(req, timeout)),
        )
        .await;

        let mut id_to_uri = std::collections::HashMap::new();
        let mut initial_responses = Vec::new();
        for ((req_id, uri_string, _req), result) in prepared.into_iter().zip(results) {
            match result {
                Ok(response) if response.msg_type == ResponseType::Error => {
                    if let Some(error) = &response.error {
                        tracing::warn!(uri = %uri_string, error, "Subscribe returned error");
                    }
                }
                Ok(response) => {
                    initial_responses.push((uri_string.clone(), response));
                    id_to_uri.insert(req_id, uri_string);
                }
                Err(e) => {
                    tracing::warn!(uri = %uri_string, error = %e, "Subscribe request failed");
                }
            }
        }

        Ok((id_to_uri, initial_responses))
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
    /// Idempotent on the `500` "already in target state" path — masked
    /// as success so direct callers (tray, GUI) don't see spurious
    /// errors when toggling a no-op. Other error codes propagate so the
    /// reliable dispatch tier can react.
    pub async fn screen_off(&mut self) -> Result<()> {
        match self.command(power::TURN_OFF_SCREEN, json!({})).await {
            Ok(_) => Ok(()),
            Err(Error::TvError(msg)) if is_idempotent_screen_error(&msg) => {
                tracing::debug!(
                    error = %msg,
                    "screen_off returned idempotent 500 (screen likely already off), treating as success",
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Turn on the screen (resume from blank).
    ///
    /// Idempotent on the `500` "already in target state" path. **Does
    /// not** mask `-102 not in screen-off substate` — that error means
    /// the TV has slipped past Screen Off into a deeper power state
    /// (Active Standby etc.) and the screen-on command was refused; the
    /// reliable dispatch tier needs the error so it can escalate to
    /// `power_on` with WoL. See `docs/plans/action-qos.md` — "Bug B".
    pub async fn screen_on(&mut self) -> Result<()> {
        match self.command(power::TURN_ON_SCREEN, json!({})).await {
            Ok(_) => Ok(()),
            Err(Error::TvError(msg)) if is_idempotent_screen_error(&msg) => {
                tracing::debug!(
                    error = %msg,
                    "screen_on returned idempotent 500 (screen likely already on), treating as success",
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
    // System info
    // -------------------------------------------------------------------------

    /// Fetch hardware identity from `ssap://system/getSystemInfo`.
    ///
    /// LG's `getSystemInfo` returns `modelName`, `serialNumber`, and
    /// related fields. Not every field is populated on every firmware —
    /// older sets may omit `serialNumber`. The returned [`HardwareId`]
    /// uses the EDID PNPID `"GSM"` for manufacturer (LG is always GSM)
    /// and fills model / serial from whichever fields the TV reports.
    pub async fn get_hardware_id(&mut self) -> Result<HardwareId> {
        let response = self.command(system::GET_SYSTEM_INFO, json!({})).await?;
        let payload = &response.payload;

        let model = payload
            .get("modelName")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let serial = payload
            .get("serialNumber")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        Ok(HardwareId {
            manufacturer: Some("GSM".to_string()),
            model,
            serial,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{FakeSsapServer, RegistrationOutcome};

    #[tokio::test]
    async fn connect_and_register_two_phase() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .expect("should connect and register");

        assert!(client.is_connected());
        assert_eq!(client.client_key(), Some("test-client-key"));
        client.close().await.unwrap();
    }

    #[tokio::test]
    async fn connect_with_existing_key_single_phase() {
        let server = FakeSsapServer::start().await;
        server.set_registration_outcome(RegistrationOutcome::AcceptImmediate {
            client_key: "returning-key".to_string(),
        });

        let mut client =
            WebOsClient::connect_url(&server.ws_url(), Some("returning-key".to_string()))
                .await
                .expect("should register immediately");

        assert_eq!(client.client_key(), Some("returning-key"));
        client.close().await.unwrap();
    }

    #[tokio::test]
    async fn registration_rejection_returns_error() {
        let server = FakeSsapServer::start().await;
        server.set_registration_outcome(RegistrationOutcome::Reject);

        let result = WebOsClient::connect_url(&server.ws_url(), None).await;
        assert!(result.is_err(), "should fail with rejected registration");
    }

    #[tokio::test]
    async fn get_power_state() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let state = client.get_power_state().await.unwrap();
        assert_eq!(state, PowerState::Active);
    }

    #[tokio::test]
    async fn set_and_get_volume() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        // set_volume should succeed (server echoes success).
        client.set_volume(50).await.unwrap();

        // get_volume returns the server's canned response.
        let info = client.get_volume().await.unwrap();
        assert_eq!(info.volume, 30); // canned from fake server
        assert!(!info.muted);
    }

    #[tokio::test]
    async fn get_inputs() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let inputs = client.get_inputs().await.unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].id, "HDMI_1");
        assert_eq!(inputs[1].id, "HDMI_2");
    }

    #[tokio::test]
    async fn switch_input() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        client.switch_input("HDMI_2").await.unwrap();
    }

    #[tokio::test]
    async fn screen_off_and_on() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        client.screen_off().await.unwrap();
        client.screen_on().await.unwrap();
    }

    /// Idempotent-UX preservation: when the TV is already in the target
    /// state, LG returns `500 <message>` and the client masks that as
    /// success so direct callers (tray, GUI) don't see spurious errors
    /// on a no-op.
    #[tokio::test]
    async fn screen_on_masks_500_idempotent_error() {
        let server = FakeSsapServer::start().await;
        server.set_command_error(
            crate::endpoints::power::TURN_ON_SCREEN,
            "500 method not allowed",
        );
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        // `500` is the "already in target state" idempotent case;
        // client should swallow it and return Ok.
        client
            .screen_on()
            .await
            .expect("500 idempotent error must be masked as success");
    }

    /// Bug B regression: `-102 not in screen-off substate` means the TV
    /// transitioned past Screen Off (to Active Standby / Standby / etc.)
    /// and the screen-on command was refused. This is NOT an idempotent
    /// success — the panel is dark and the caller needs to escalate to
    /// `power_on` with WoL. The original implementation masked every
    /// `TvError(_)` as success, hiding this signal from the reliable
    /// dispatch tier. See `docs/plans/action-qos.md` — "Bug B".
    #[tokio::test]
    async fn screen_on_propagates_minus_102_not_in_screen_off_substate() {
        let server = FakeSsapServer::start().await;
        server.set_command_error(
            crate::endpoints::power::TURN_ON_SCREEN,
            "-102 not in screen-off substate",
        );
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let err = client
            .screen_on()
            .await
            .expect_err("`-102` must propagate as Err so the reliable tier can escalate");
        match err {
            Error::TvError(msg) => assert!(
                msg.contains("-102"),
                "expected error to carry the -102 code, got {msg:?}",
            ),
            other => panic!("expected Error::TvError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn power_off() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        client.power_off().await.unwrap();
    }

    #[tokio::test]
    async fn get_backlight() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let backlight = client.get_backlight().await.unwrap();
        assert_eq!(backlight, 80); // canned from fake server
    }

    #[tokio::test]
    async fn set_backlight_via_luna_workaround() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        // set_backlight uses createAlert + closeAlert workaround.
        client.set_backlight(60).await.unwrap();
    }

    #[tokio::test]
    async fn get_state_aggregates_all_endpoints() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let state = client.get_state().await.unwrap();
        assert!(state.is_on);
        assert!(state.is_screen_on);
        assert_eq!(state.volume, Some(30));
        assert_eq!(state.muted, Some(false));
        assert_eq!(state.backlight, Some(80));
        assert!(!state.inputs.is_empty());
    }

    #[tokio::test]
    async fn subscribe_state_updates() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let (id_to_uri, initial_responses) = client.subscribe_state_updates().await.unwrap();

        // Should have subscriptions for power, volume, mute, inputs,
        // current input, and backlight (6 total).
        assert_eq!(id_to_uri.len(), 6, "id_to_uri: {id_to_uri:?}");
        assert_eq!(initial_responses.len(), 6);
    }

    #[tokio::test]
    async fn subscription_push_arrives_on_channel() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        let (id_to_uri, _) = client.subscribe_state_updates().await.unwrap();
        let mut sub_rx = client.take_subscription_rx().expect("should have rx");

        // Find the request ID for the volume subscription.
        let vol_req_id = id_to_uri
            .iter()
            .find(|(_, uri)| uri.contains("getVolume"))
            .map(|(id, _)| id.clone())
            .expect("should have volume subscription");

        // Push a volume update from the server.
        server.push_subscription_update(
            &vol_req_id,
            serde_json::json!({
                "returnValue": true,
                "volumeStatus": { "volume": 75, "muted": true }
            }),
        );

        // The update should arrive on the subscription channel.
        let update = tokio::time::timeout(std::time::Duration::from_secs(2), sub_rx.recv())
            .await
            .expect("should receive within 2s")
            .expect("channel should not be closed");

        assert_eq!(update.id.as_deref(), Some(vol_req_id.as_str()));
    }

    #[tokio::test]
    async fn mute_control() {
        let server = FakeSsapServer::start().await;
        let mut client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        client.set_mute(true).await.unwrap();
        let muted = client.get_mute().await.unwrap();
        assert!(!muted); // canned response from fake server
    }
}
