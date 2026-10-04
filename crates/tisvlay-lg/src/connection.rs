//! LG WebOS TV connection — the [`TvConnection`] implementation.
//!
//! [`LgConnection`] wraps a [`WebOsClient`](crate::client::WebOsClient)
//! and forwards the feature methods onto SSAP commands, reconnecting
//! transparently when the TV drops the idle WebSocket. It also holds
//! the SSAP-payload → [`TvState`] parsing helpers, one of which
//! ([`parse_power_state_payload`]) is shared with the client's
//! synchronous power query.

use crate::client::WebOsClient;
use crate::config::LgVendorConfig;
use async_trait::async_trait;
use serde_json::Value;
use tisvlay_core::{Capabilities, Error, InputInfo, PowerState, Result, TvConnection, TvState};
use tokio::sync::mpsc;
use tracing::{info, trace, warn};

/// LG WebOS TV connection.
///
/// Wraps a `WebOsClient` and implements the tisvlay-core connection traits.
/// Handles auto-reconnection transparently when the WebSocket connection drops.
#[derive(Debug)]
pub struct LgConnection {
    client: Option<WebOsClient>,
    config: LgVendorConfig,
    capabilities: Capabilities,
    /// Cached hardware identity. Seeded by the connector at construction
    /// time (after the initial `getSystemInfo` call) and refreshed on
    /// each fresh reconnect. Returned from [`hardware_id`](Self::hardware_id)
    /// without blocking.
    hardware_id: std::sync::Arc<std::sync::RwLock<Option<tisvlay_core::HardwareId>>>,
    /// Test override for where a reconnect dials — the fake SSAP server's
    /// URL, since the real path always dials the configured IP on the
    /// standard SSAP ports.
    #[cfg(test)]
    reconnect_url: Option<String>,
}

impl LgConnection {
    /// Create a new LG connection from a WebOsClient and its config.
    ///
    /// `hardware_id` is the identity read during the connect flow (or
    /// `None` if the TV didn't respond to `getSystemInfo` — rare on
    /// supported firmware). Exposed via
    /// [`hardware_id`](Self::hardware_id) without blocking on I/O.
    pub fn new(
        client: WebOsClient,
        config: LgVendorConfig,
        hardware_id: Option<tisvlay_core::HardwareId>,
    ) -> Self {
        Self {
            client: Some(client),
            config,
            capabilities: Capabilities::STOCK,
            hardware_id: std::sync::Arc::new(std::sync::RwLock::new(hardware_id)),
            #[cfg(test)]
            reconnect_url: None,
        }
    }

    /// Ask the TV for its hardware identity and cache the result.
    ///
    /// Silently swallows errors (e.g. legacy firmware without
    /// `getSystemInfo`) so a failed lookup doesn't stall reconnect.
    async fn refresh_hardware_id(&mut self) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match client.get_hardware_id().await {
            Ok(id) => {
                tracing::debug!(ip = %self.config.ip, ?id, "fetched LG hardware identity");
                if let Ok(mut guard) = self.hardware_id.write() {
                    *guard = Some(id);
                }
            }
            Err(e) => {
                tracing::debug!(ip = %self.config.ip, error = %e, "getSystemInfo failed, skipping hardware_id cache");
            }
        }
    }

    /// Open and register a fresh client for the configured TV.
    async fn dial(&self) -> Result<WebOsClient> {
        #[cfg(test)]
        if let Some(url) = &self.reconnect_url {
            return WebOsClient::connect_url(url, self.config.client_key.clone()).await;
        }
        WebOsClient::connect_with_scheme(
            &self.config.ip,
            self.config.scheme,
            self.config.client_key.clone(),
        )
        .await
    }

    /// Ensure the WebSocket connection is alive, reconnecting if needed.
    async fn ensure_connected(&mut self) -> Result<&mut WebOsClient> {
        let needs_reconnect = match &self.client {
            Some(client) => !client.is_connected(),
            None => true,
        };

        if needs_reconnect {
            info!(ip = %self.config.ip, scheme = %self.config.scheme, "Reconnecting to LG TV");
            let client = self.dial().await?;
            self.client = Some(client);

            // Best-effort identity refresh on fresh connect. Runs once
            // per reconnect (the values don't change for a given TV).
            if self.hardware_id.read().map(|g| g.is_none()).unwrap_or(true) {
                self.refresh_hardware_id().await;
            }
        }

        self.client.as_mut().ok_or(Error::ConnectionClosed)
    }

    /// Force a fresh reconnect (drop existing connection first).
    async fn reconnect(&mut self) -> Result<&mut WebOsClient> {
        self.client = None;
        self.ensure_connected().await
    }

    /// Execute a command with automatic retry on stale connections.
    ///
    /// The TV silently closes idle WebSocket connections after ~1-2 minutes.
    /// `is_connected()` can't detect this — the handle looks valid until we
    /// try to send data. This method catches the resulting error, reconnects,
    /// and retries the command once.
    async fn with_retry<F, T>(&mut self, mut f: F) -> Result<T>
    where
        F: FnMut(
            &mut WebOsClient,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + '_>>,
    {
        let client = self.ensure_connected().await?;
        match f(client).await {
            Ok(result) => Ok(result),
            Err(e)
                if matches!(
                    e,
                    Error::ConnectionClosed
                        | Error::Connection(_)
                        | Error::Io(_)
                        | Error::RequestTimeout
                ) =>
            {
                warn!(ip = %self.config.ip, error = %e, "Stale connection, reconnecting");
                let client = self.reconnect().await?;
                f(client).await
            }
            Err(e) => Err(e),
        }
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
        self.with_retry(|c| Box::pin(c.get_state())).await
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

    fn hardware_id(&self) -> Option<tisvlay_core::HardwareId> {
        self.hardware_id.read().ok().and_then(|g| g.clone())
    }

    /// The TV reports interface addresses as IPv4, so only an IPv4 dial
    /// can be matched; a hostname or IPv6 dial reports nothing. Uses the
    /// live client as is — a best-effort read, not a command worth a
    /// reconnect.
    async fn get_link_mac(&mut self) -> Option<tisvlay_core::MacAddress> {
        let Ok(dialed) = self.config.ip.parse::<std::net::Ipv4Addr>() else {
            tracing::debug!(ip = %self.config.ip, "link MAC needs an IPv4 dial address");
            return None;
        };
        self.client.as_mut()?.get_link_mac(dialed.into()).await
    }

    // =========================================================================
    // Feature methods (overriding TvConnection defaults)
    // =========================================================================

    async fn power_off(&mut self) -> Result<()> {
        if let Some(ref mut client) = self.client {
            if let Err(e) = client.power_off().await {
                tracing::warn!(error = %e, "power_off command failed (TV may already be off)");
            }
            let _ = client.close().await;
        }
        self.client = None;
        Ok(())
    }

    async fn get_power_state(&mut self) -> Result<PowerState> {
        self.with_retry(|c| Box::pin(c.get_power_state())).await
    }

    async fn screen_on(&mut self) -> Result<()> {
        self.with_retry(|c| Box::pin(c.screen_on())).await
    }

    async fn screen_off(&mut self) -> Result<()> {
        self.with_retry(|c| Box::pin(c.screen_off())).await
    }

    async fn is_screen_on(&mut self) -> Result<bool> {
        let state = self.with_retry(|c| Box::pin(c.get_power_state())).await?;
        // Mirrors the positive whitelist in `TvState::update_power` —
        // `Active` / `ScreenSaver` only. Future `#[non_exhaustive]`
        // variants default to `false` (safer) and the two derivations
        // can't drift.
        Ok(matches!(
            state,
            PowerState::Active | PowerState::ScreenSaver
        ))
    }

    async fn set_volume(&mut self, level: u8) -> Result<()> {
        self.with_retry(|c| Box::pin(c.set_volume(level))).await
    }

    async fn get_volume(&mut self) -> Result<u8> {
        let info = self.with_retry(|c| Box::pin(c.get_volume())).await?;
        Ok(info.volume.clamp(0, 100) as u8)
    }

    async fn set_mute(&mut self, mute: bool) -> Result<()> {
        self.with_retry(|c| Box::pin(c.set_mute(mute))).await
    }

    async fn is_muted(&mut self) -> Result<bool> {
        let info = self.with_retry(|c| Box::pin(c.get_volume())).await?;
        Ok(info.muted)
    }

    async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        let input = input_id.to_string();
        self.with_retry(|c| {
            let input = input.clone();
            Box::pin(async move { c.switch_input(&input).await })
        })
        .await
    }

    async fn get_current_input(&mut self) -> Result<Option<String>> {
        let state = self.with_retry(|c| Box::pin(c.get_state())).await?;
        Ok(state.current_input)
    }

    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        self.with_retry(|c| Box::pin(c.get_inputs())).await
    }

    async fn set_backlight(&mut self, level: u8) -> Result<()> {
        self.with_retry(|c| Box::pin(c.set_backlight(level))).await
    }

    async fn get_backlight(&mut self) -> Result<u8> {
        self.with_retry(|c| Box::pin(c.get_backlight())).await
    }

    async fn subscribe_state(&mut self) -> Result<mpsc::UnboundedReceiver<TvState>> {
        let client = self.ensure_connected().await?;

        // Subscribe to SSAP state endpoints. Returns a mapping of request
        // ID → URI so we can identify which subscription each update belongs
        // to (SSAP updates carry the request ID but not the endpoint URI),
        // plus the raw initial responses for seeding the cached state.
        let (id_to_uri, initial_responses) = client.subscribe_state_updates().await?;

        // Take the subscription receiver from the client.
        let sub_rx = client
            .take_subscription_rx()
            .ok_or(Error::ConnectionClosed)?;

        // Seed the cached state from initial subscription responses so the
        // first push update produces a complete snapshot instead of a partial
        // one built from TvState::new().
        let mut initial_state = TvState::new();
        for (uri, response) in &initial_responses {
            update_state_from_subscription(&mut initial_state, uri, &response.payload);
        }

        // Create a channel for TvState updates.
        let (state_tx, state_rx) = mpsc::unbounded_channel();

        // Send the initial seeded state so D-Bus cached properties are
        // populated immediately, rather than waiting for the first
        // subsequent WebSocket push from the TV.
        let _ = state_tx.send(initial_state.clone());

        // Spawn a task that converts SSAP subscription responses into
        // TvState snapshots. Each update merges into a cached state so
        // the receiver always gets a complete snapshot.
        tokio::spawn(async move {
            let mut cached_state = initial_state;
            let mut sub_rx = sub_rx;
            while let Some(response) = sub_rx.recv().await {
                // Look up the endpoint URI from the response ID.
                let uri = response
                    .id
                    .as_ref()
                    .and_then(|id| id_to_uri.get(id))
                    .map(|s| s.as_str());

                if let Some(uri) = uri {
                    let changed =
                        update_state_from_subscription(&mut cached_state, uri, &response.payload);
                    if changed && state_tx.send(cached_state.clone()).is_err() {
                        // Receiver dropped — stop processing.
                        break;
                    }
                }
            }
            trace!("Subscription state converter task exited");
        });

        Ok(state_rx)
    }
}

/// Parse an LG `getPowerState` payload to a [`PowerState`].
///
/// LG's payload carries three signals:
///
/// - `state`: settled label ("Active", "Active Standby", ...)
/// - `processing`: in-flight transition name ("Request Power Off",
///   "Request Active Standby", "Prepare Active Standby", and the
///   literal "None" when idle).
/// - `onOff`: authoritative "on" / "off" hint.
///
/// During a remote-press power-off, LG keeps `state="Active"` for
/// ~3 s while the transition completes, but starts setting
/// `processing` and `onOff="off"` within ~200 ms of the button
/// press. Trust the discriminators over the `state` lie so callers
/// see the TV's actual substate at the moment of the press, not
/// three seconds later. See `scripts/probe_power_state_timing.py`
/// output for the captured payloads that motivated this.
///
/// Returns `None` if the payload contains no usable state info.
pub(crate) fn parse_power_state_payload(payload: &Value) -> Option<PowerState> {
    let on_off = payload.get("onOff").and_then(|v| v.as_str());
    let processing = payload.get("processing").and_then(|v| v.as_str());
    let in_transition =
        on_off == Some("off") || matches!(processing, Some(p) if !p.eq_ignore_ascii_case("None"));

    if in_transition {
        // Landing on `ActiveStandby` is sufficient for the daemon's
        // wake pre-flight to fire on the off-direction; we don't
        // enumerate every `processing` string. The on-direction
        // (waking up) updates `state` fast enough for the
        // state-field fall-through below to handle.
        return Some(PowerState::ActiveStandby);
    }
    payload
        .get("state")
        .and_then(|v| v.as_str())
        .map(PowerState::parse)
}

/// Update a cached [`TvState`] from an SSAP subscription response.
///
/// Returns `true` if the state was actually modified.
fn update_state_from_subscription(state: &mut TvState, uri: &str, payload: &Value) -> bool {
    if uri.contains("getPowerState") {
        if let Some(power) = parse_power_state_payload(payload) {
            state.update_power(power);
            return true;
        }
    } else if uri.contains("getVolume") {
        if let Some(vol) = payload.get("volume").and_then(|v| v.as_i64()) {
            state.volume = Some(vol.clamp(0, 100) as u8);
            // Some firmware includes mute in volume updates.
            if let Some(muted) = payload.get("mute").and_then(|v| v.as_bool()) {
                state.muted = Some(muted);
            }
            return true;
        }
        // Also check nested volumeStatus format (subscription updates use this).
        if let Some(status) = payload.get("volumeStatus") {
            if let Some(vol) = status.get("volume").and_then(|v| v.as_i64()) {
                state.volume = Some(vol.clamp(0, 100) as u8);
            }
            // The field is "muteStatus" (bool) in volumeStatus, not "muted".
            if let Some(muted) = status.get("muteStatus").and_then(|v| v.as_bool()) {
                state.muted = Some(muted);
            }
            return true;
        }
    } else if uri.contains("getMute") {
        if let Some(muted) = payload.get("mute").and_then(|v| v.as_bool()) {
            state.muted = Some(muted);
            return true;
        }
    } else if uri.contains("getExternalInputList") {
        if let Some(devices) = payload.get("devices").and_then(|v| v.as_array()) {
            state.input_app_id_map.clear();
            state.inputs = devices
                .iter()
                .filter_map(|d| {
                    let id = d.get("id")?.as_str()?;
                    let label = d.get("label")?.as_str()?;
                    let connected = d
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // Build reverse mapping: app ID → physical input ID.
                    // LG reports inputs as apps (e.g., "com.webos.app.hdmi1")
                    // in getForegroundAppInfo, but uses physical IDs (e.g.,
                    // "HDMI_1") in the input list. This map normalizes them.
                    if let Some(app_id) = d.get("appId").and_then(|v| v.as_str()) {
                        state
                            .input_app_id_map
                            .insert(app_id.to_string(), id.to_string());
                    }
                    Some(InputInfo {
                        id: id.to_string(),
                        label: label.to_string(),
                        connected,
                    })
                })
                .collect();
            return true;
        }
    } else if uri.contains("getForegroundAppInfo") {
        if let Some(app_id) = payload.get("appId").and_then(|v| v.as_str()) {
            state.current_app = Some(app_id.to_string());
            // Normalize app ID to physical input ID using the mapping
            // built from getExternalInputList. Falls back to the raw app
            // ID if no mapping exists (e.g., non-input apps like Netflix).
            let input_id = state
                .input_app_id_map
                .get(app_id)
                .cloned()
                .unwrap_or_else(|| app_id.to_string());
            state.current_input = Some(input_id);
            return true;
        }
    } else if uri.contains("#picture/backlight") {
        // Backlight subscription via ssap://settings/getSystemSettings
        // with category "picture", keys ["backlight"].
        if let Some(bl) = payload
            .get("settings")
            .and_then(|s| s.get("backlight"))
            .and_then(|v| v.as_i64())
        {
            state.backlight = Some(bl.clamp(0, 100) as u8);
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // update_state_from_subscription
    // -----------------------------------------------------------------------

    #[test]
    fn subscription_power_active() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.service.tvpower/power/getPowerState",
            &json!({"state": "Active", "processing": "None"}),
        );
        assert!(changed);
        assert!(state.is_on);
        assert!(state.is_screen_on);
    }

    #[test]
    fn subscription_power_screen_off() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.service.tvpower/power/getPowerState",
            &json!({"state": "Screen Off"}),
        );
        assert!(changed);
        assert!(state.is_on);
        assert!(!state.is_screen_on);
    }

    #[test]
    fn subscription_power_standby() {
        let mut state = TvState::new();
        state.is_on = true;
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.service.tvpower/power/getPowerState",
            &json!({"state": "Standby"}),
        );
        assert!(changed);
        assert!(!state.is_on);
    }

    /// Captured from the live TV via
    /// `scripts/probe_power_state_timing.py`: ~191 ms after a
    /// remote-press power-off, LG pushes a getPowerState update
    /// that keeps `state="Active"` (still lying) but adds
    /// `processing="Request Power Off"` + `onOff="off"`. The parser
    /// must project to `ActiveStandby` immediately so the daemon's
    /// reliable-dispatch pre-flight sees a non-Active substate and
    /// wakes the TV before sending `screen_off`.
    #[test]
    fn subscription_power_transition_to_off_projects_active_standby() {
        for processing in [
            "Request Power Off",
            "Request Active Standby",
            "Prepare Active Standby",
        ] {
            let mut state = TvState::new();
            state.update_power(PowerState::Active);
            let changed = update_state_from_subscription(
                &mut state,
                "ssap://com.webos.service.tvpower/power/getPowerState",
                &json!({
                    "state": "Active",
                    "processing": processing,
                    "onOff": "off",
                    "reason": "remoteKey",
                }),
            );
            assert!(changed, "processing={processing}");
            assert_eq!(
                state.power_state,
                PowerState::ActiveStandby,
                "processing={processing} should project to ActiveStandby \
                 regardless of the stale state=Active field",
            );
            assert!(state.is_on, "ActiveStandby keeps NIC alive (is_on=true)");
            assert!(!state.is_screen_on, "ActiveStandby is panel-off");
        }
    }

    /// `onOff="off"` alone, without `processing`, still triggers the
    /// transition projection — the field is authoritative.
    #[test]
    fn subscription_power_on_off_field_alone_projects_active_standby() {
        let mut state = TvState::new();
        state.update_power(PowerState::Active);
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.service.tvpower/power/getPowerState",
            &json!({"state": "Active", "onOff": "off"}),
        );
        assert!(changed);
        assert_eq!(state.power_state, PowerState::ActiveStandby);
    }

    /// The literal `processing="None"` (LG's "idle, not transitioning"
    /// sentinel) must NOT trigger the transition projection — it
    /// means "no transition in flight", not "transitioning to None".
    #[test]
    fn subscription_power_processing_none_falls_through_to_state_field() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.service.tvpower/power/getPowerState",
            &json!({"state": "Active", "processing": "None"}),
        );
        assert!(changed);
        assert_eq!(state.power_state, PowerState::Active);
        assert!(state.is_on);
        assert!(state.is_screen_on);
    }

    /// Settled `state="Active Standby"` (no `processing` field, no
    /// `onOff`) still routes through the existing state-field
    /// parser to `ActiveStandby`. This is what LG pushes ~3 s after
    /// the transition starts.
    #[test]
    fn subscription_power_settled_active_standby_via_state_field() {
        let mut state = TvState::new();
        state.update_power(PowerState::Active);
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.service.tvpower/power/getPowerState",
            &json!({"state": "Active Standby"}),
        );
        assert!(changed);
        assert_eq!(state.power_state, PowerState::ActiveStandby);
    }

    // -----------------------------------------------------------------------
    // parse_power_state_payload — used by both `update_state_from_subscription`
    // (above) and `WebOsClient::get_power_state` (synchronous query). Pin
    // the helper directly so the contract is unambiguous for both callers.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_payload_state_field_only() {
        assert_eq!(
            parse_power_state_payload(&json!({"state": "Active"})),
            Some(PowerState::Active),
        );
        assert_eq!(
            parse_power_state_payload(&json!({"state": "Standby"})),
            Some(PowerState::Standby),
        );
    }

    #[test]
    fn parse_payload_discriminators_override_state() {
        // The captured probe payload — `state` lies but `processing` +
        // `onOff` are authoritative.
        assert_eq!(
            parse_power_state_payload(&json!({
                "state": "Active",
                "processing": "Request Power Off",
                "onOff": "off",
            })),
            Some(PowerState::ActiveStandby),
        );
    }

    #[test]
    fn parse_payload_none_sentinel_does_not_trigger_transition() {
        assert_eq!(
            parse_power_state_payload(&json!({
                "state": "Active",
                "processing": "None",
            })),
            Some(PowerState::Active),
        );
    }

    #[test]
    fn parse_payload_empty_returns_none() {
        assert_eq!(parse_power_state_payload(&json!({})), None);
    }

    #[test]
    fn subscription_volume_nested() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://audio/getVolume",
            &json!({"volumeStatus": {"volume": 42, "muteStatus": true}}),
        );
        assert!(changed);
        assert_eq!(state.volume, Some(42));
        assert_eq!(state.muted, Some(true));
    }

    #[test]
    fn subscription_volume_flat() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://audio/getVolume",
            &json!({"volume": 75, "mute": false}),
        );
        assert!(changed);
        assert_eq!(state.volume, Some(75));
        assert_eq!(state.muted, Some(false));
    }

    #[test]
    fn subscription_mute() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://audio/getMute",
            &json!({"mute": true}),
        );
        assert!(changed);
        assert_eq!(state.muted, Some(true));
    }

    #[test]
    fn subscription_input_list() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://tv/getExternalInputList",
            &json!({
                "devices": [
                    {"id": "HDMI_1", "label": "PC", "connected": true, "appId": "com.webos.app.hdmi1"},
                    {"id": "HDMI_2", "label": "Console", "connected": false, "appId": "com.webos.app.hdmi2"},
                ]
            }),
        );
        assert!(changed);
        assert_eq!(state.inputs.len(), 2);
        assert_eq!(state.inputs[0].id, "HDMI_1");
        assert!(state.inputs[0].connected);
        // App ID map should be built for foreground app normalization.
        assert_eq!(
            state.input_app_id_map.get("com.webos.app.hdmi1"),
            Some(&"HDMI_1".to_string())
        );
    }

    #[test]
    fn subscription_foreground_app_normalizes_to_input_id() {
        let mut state = TvState::new();
        // First, build the app ID → input ID mapping.
        update_state_from_subscription(
            &mut state,
            "ssap://tv/getExternalInputList",
            &json!({
                "devices": [
                    {"id": "HDMI_1", "label": "PC", "connected": true, "appId": "com.webos.app.hdmi1"},
                ]
            }),
        );
        // Then, foreground app update should map app ID to physical input.
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.applicationManager/getForegroundAppInfo",
            &json!({"appId": "com.webos.app.hdmi1"}),
        );
        assert!(changed);
        assert_eq!(state.current_input.as_deref(), Some("HDMI_1"));
        assert_eq!(state.current_app.as_deref(), Some("com.webos.app.hdmi1"));
    }

    #[test]
    fn subscription_foreground_app_unknown_app_passes_through() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://com.webos.applicationManager/getForegroundAppInfo",
            &json!({"appId": "com.webos.app.netflix"}),
        );
        assert!(changed);
        // No mapping exists, so the raw app ID is used as current_input.
        assert_eq!(
            state.current_input.as_deref(),
            Some("com.webos.app.netflix")
        );
    }

    #[test]
    fn subscription_backlight() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://settings/getSystemSettings#picture/backlight",
            &json!({"settings": {"backlight": 65}}),
        );
        assert!(changed);
        assert_eq!(state.backlight, Some(65));
    }

    #[test]
    fn subscription_unknown_uri_returns_false() {
        let mut state = TvState::new();
        let changed = update_state_from_subscription(
            &mut state,
            "ssap://unknown/endpoint",
            &json!({"something": true}),
        );
        assert!(!changed);
    }

    #[test]
    fn subscription_volume_clamps_to_100() {
        let mut state = TvState::new();
        update_state_from_subscription(
            &mut state,
            "ssap://audio/getVolume",
            &json!({"volume": 200}),
        );
        assert_eq!(state.volume, Some(100));
    }

    // -----------------------------------------------------------------------
    // Device-reported link MAC
    // -----------------------------------------------------------------------

    const WIRED_MAC: &str = "64:E4:A5:6C:AB:B4";
    const WIFI_MAC: &str = "84:90:0A:63:A9:94";

    /// Script the TV's network-state replies: which interface is connected
    /// at which address, plus the per-interface MACs (a Wi-Fi Direct block
    /// included — it must never be picked).
    fn script_network(server: &crate::test_server::FakeSsapHandle, iface: &str, ip: &str) {
        use crate::endpoints::network;
        let mut status = json!({
            "returnValue": true,
            "wired": { "plugged": false, "state": "disconnected" },
            "wifi": { "state": "disconnected" },
            "wifiDirect": { "state": "disconnected" },
        });
        status[iface] = json!({ "state": "connected", "ipAddress": ip });
        server.set_command_payload(network::GET_STATUS, status);
        server.set_command_payload(
            network::GET_INFO,
            json!({
                "returnValue": true,
                "wiredInfo": { "macAddress": WIRED_MAC.to_lowercase() },
                "wifiInfo": { "macAddress": WIFI_MAC },
                "p2pInfo": { "macAddress": "86:90:0A:63:A9:94" },
            }),
        );
    }

    async fn link_mac_with(
        server: &crate::test_server::FakeSsapHandle,
        dial: &str,
    ) -> Option<String> {
        let client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();
        let mut conn = LgConnection::new(client, LgVendorConfig::new(dial), None);
        conn.get_link_mac().await.map(|mac| mac.to_string())
    }

    #[tokio::test]
    async fn link_mac_is_the_connected_interface_carrying_the_dialed_address() {
        let server = crate::test_server::FakeSsapServer::start().await;
        script_network(&server, "wired", "127.0.0.1");
        assert_eq!(
            link_mac_with(&server, "127.0.0.1").await.as_deref(),
            Some(WIRED_MAC)
        );
        script_network(&server, "wifi", "127.0.0.1");
        assert_eq!(
            link_mac_with(&server, "127.0.0.1").await.as_deref(),
            Some(WIFI_MAC)
        );
    }

    #[tokio::test]
    async fn link_mac_is_none_when_no_interface_matches_the_dial() {
        let server = crate::test_server::FakeSsapServer::start().await;
        script_network(&server, "wifi", "10.0.0.9");
        assert_eq!(link_mac_with(&server, "127.0.0.1").await, None);
    }

    #[tokio::test]
    async fn link_mac_is_none_when_the_tv_refuses() {
        use crate::endpoints::network;
        let server = crate::test_server::FakeSsapServer::start().await;
        script_network(&server, "wired", "127.0.0.1");
        server.set_command_payload(network::GET_INFO, json!({ "returnValue": false }));
        assert_eq!(link_mac_with(&server, "127.0.0.1").await, None);
        server.set_command_error(network::GET_STATUS, "404 no such service or method");
        assert_eq!(link_mac_with(&server, "127.0.0.1").await, None);
    }

    #[tokio::test]
    async fn link_mac_needs_an_ipv4_dial_address() {
        let server = crate::test_server::FakeSsapServer::start().await;
        script_network(&server, "wired", "127.0.0.1");
        assert_eq!(link_mac_with(&server, "tv.local").await, None);
        assert_eq!(link_mac_with(&server, "::1").await, None);
    }

    // -----------------------------------------------------------------------
    // LgConnection via fake server
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn lg_connection_get_state() {
        let server = crate::test_server::FakeSsapServer::start().await;
        let client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();
        let config = LgVendorConfig::new("127.0.0.1");
        let mut conn = LgConnection::new(client, config, None);

        let state = conn.get_state().await.unwrap();
        assert!(state.is_on);
        assert_eq!(state.volume, Some(30));
        assert_eq!(state.backlight, Some(80));
    }

    #[tokio::test]
    async fn lg_connection_subscribe_state_produces_initial_snapshot() {
        let server = crate::test_server::FakeSsapServer::start().await;
        let client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();
        let config = LgVendorConfig::new("127.0.0.1");
        let mut conn = LgConnection::new(client, config, None);

        let mut rx = conn.subscribe_state().await.unwrap();

        // The initial snapshot should arrive immediately.
        let state = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive within 2s")
            .expect("channel open");

        assert!(state.is_on);
        assert_eq!(state.volume, Some(30));
        assert!(!state.inputs.is_empty());
    }

    #[tokio::test]
    async fn with_retry_reconnects_after_drop() {
        let server = crate::test_server::FakeSsapServer::start().await;
        let client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();

        // An LgConnection on the fake server, its reconnect aimed back at
        // it: the real reconnect path dials the configured IP on the
        // standard SSAP ports, which the fake server isn't on.
        let mut config = LgVendorConfig::new(server.addr().ip().to_string());
        config.client_key = Some("test-client-key".to_string());
        let mut conn = LgConnection::new(client, config, None);
        conn.reconnect_url = Some(server.ws_url());

        // First call succeeds.
        let state = conn.get_state().await.unwrap();
        assert!(state.is_on);

        // Server drops connection.
        server.drop_connection();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // with_retry detects the stale connection, reconnects — aimed at
        // the fake server, which accepts a fresh connection — and the
        // retried command succeeds.
        let state = conn.get_state().await.expect("reconnected and retried");
        assert!(state.is_on);
    }

    #[tokio::test]
    async fn lg_connection_trait_methods() {
        let server = crate::test_server::FakeSsapServer::start().await;
        let client = WebOsClient::connect_url(&server.ws_url(), None)
            .await
            .unwrap();
        let config = LgVendorConfig::new("127.0.0.1");
        let mut conn = LgConnection::new(client, config, None);

        // Exercise each TvConnection method.
        assert_eq!(conn.vendor_id(), "lg");
        assert!(conn.is_connected());
        assert_eq!(conn.capabilities(), Capabilities::STOCK);

        conn.screen_off().await.unwrap();
        conn.screen_on().await.unwrap();
        conn.set_volume(50).await.unwrap();
        assert_eq!(conn.get_volume().await.unwrap(), 30); // canned
        conn.set_mute(true).await.unwrap();
        conn.switch_input("HDMI_2").await.unwrap();
        conn.set_backlight(60).await.unwrap();
        assert_eq!(conn.get_backlight().await.unwrap(), 80); // canned

        let inputs = conn.list_inputs().await.unwrap();
        assert_eq!(inputs.len(), 2);

        conn.close().await.unwrap();
        assert!(!conn.is_connected());
    }
}
