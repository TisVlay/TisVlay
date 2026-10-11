//! SSAP API endpoint constants.
//!
//! These endpoints are used to control LG WebOS TVs via the SSAP protocol.

/// System metadata endpoints.
pub(crate) mod system {
    /// Get system info (model name, serial number, etc.).
    pub(crate) const GET_SYSTEM_INFO: &str = "ssap://system/getSystemInfo";
}

/// Network-state endpoints (need `READ_NETWORK_STATE`).
pub(crate) mod network {
    /// Per-interface MACs: `wiredInfo` / `wifiInfo` / `p2pInfo`.
    pub(crate) const GET_INFO: &str = "ssap://com.webos.service.connectionmanager/getinfo";

    /// Per-interface link state and addressing (`wired` / `wifi` blocks with
    /// `state` and `ipAddress`). Lowercase: the camel-case `getStatus` 404s.
    pub(crate) const GET_STATUS: &str = "ssap://com.webos.service.connectionmanager/getstatus";
}

/// Power control endpoints.
pub(crate) mod power {
    /// Turn off the TV (standby).
    pub(crate) const TURN_OFF: &str = "ssap://system/turnOff";

    /// Get current power state.
    pub(crate) const GET_POWER_STATE: &str = "ssap://com.webos.service.tvpower/power/getPowerState";

    /// Turn off the screen (OLED-safe blank, TV stays on).
    pub(crate) const TURN_OFF_SCREEN: &str = "ssap://com.webos.service.tvpower/power/turnOffScreen";

    /// Turn on the screen (resume from blank).
    pub(crate) const TURN_ON_SCREEN: &str = "ssap://com.webos.service.tvpower/power/turnOnScreen";
}

/// Input/source control endpoints.
pub(crate) mod input {
    /// Switch to a specific input.
    /// Payload: `{"inputId": "HDMI_1"}`
    pub(crate) const SWITCH_INPUT: &str = "ssap://tv/switchInput";

    /// Get list of available external inputs.
    pub(crate) const GET_EXTERNAL_INPUT_LIST: &str = "ssap://tv/getExternalInputList";

    /// Get the current input.
    pub(crate) const GET_CURRENT_INPUT: &str =
        "ssap://com.webos.applicationManager/getForegroundAppInfo";
}

/// Audio control endpoints.
pub(crate) mod audio {
    /// Set volume level.
    /// Payload: `{"volume": 50}`
    pub(crate) const SET_VOLUME: &str = "ssap://audio/setVolume";

    /// Get current volume.
    pub(crate) const GET_VOLUME: &str = "ssap://audio/getVolume";

    /// Set mute state.
    /// Payload: `{"mute": true}`
    pub(crate) const SET_MUTE: &str = "ssap://audio/setMute";

    /// Get mute state.
    pub(crate) const GET_MUTE: &str = "ssap://audio/getMute";
}

/// Picture/display settings endpoints.
pub(crate) mod picture {
    /// Get system settings by category.
    /// Payload: `{"category": "picture", "keys": ["contrast", "backlight", "brightness", "color"]}`
    pub(crate) const GET_SYSTEM_SETTINGS: &str = "ssap://settings/getSystemSettings";

    // NOTE: SET_SYSTEM_SETTINGS (ssap://settings/setSystemSettings) returns
    // "401 insufficient permissions" on some firmware. We use the Luna
    // createAlert workaround for writes (see client.rs set_backlight).
    // GET_SYSTEM_SETTINGS works with READ_SETTINGS in the manifest (added
    // to the signed manifest section — requires re-pairing after update).
}

/// Notification endpoints.
pub(crate) mod notification {
    /// Create an alert with buttons/actions.
    /// Used as a workaround to call Luna services that require elevated permissions.
    pub(crate) const CREATE_ALERT: &str = "ssap://system.notifications/createAlert";

    /// Close an alert by ID.
    /// Payload: `{"alertId": "com.webos.service.apiadapter-12345"}`
    pub(crate) const CLOSE_ALERT: &str = "ssap://system.notifications/closeAlert";
}

/// Luna service URIs (used via createAlert workaround).
///
/// # Background
///
/// The SSAP settings API (`ssap://settings/setSystemSettings`) returns
/// "401 insufficient permissions" for certain settings like backlight,
/// even when `READ_SETTINGS` and `WRITE_SETTINGS` permissions are in the
/// manifest and the TV has been re-paired.
///
/// LGTVCompanion discovered a workaround: wrap Luna service calls inside
/// a `createAlert` notification. The alert's `onclose`/`onfail` handlers
/// can invoke Luna services directly, bypassing SSAP permission checks.
///
/// # Usage
///
/// 1. Send `createAlert` with Luna URI in `buttons[].onClick`, `onclose.uri`, `onfail.uri`
/// 2. Extract `alertId` from response
/// 3. Immediately send `closeAlert` with that `alertId` to dismiss without user interaction
///
/// See `WebOsClient::set_backlight()` for implementation.
///
/// # Future Work
///
/// If LG fixes the permissions issue, we could try:
/// - Adding `WRITE_SETTINGS` permission to manifest
/// - Re-pairing with TV
/// - Using `ssap://settings/setSystemSettings` directly
/// - Or the calibration API: `ssap://externalpq/setExternalPqData` (used by bscpylgtv)
pub(crate) mod luna {
    /// Set system settings via Luna service.
    /// Payload: `{"category": "picture", "settings": {"backlight": 50}}`
    pub(crate) const SET_SYSTEM_SETTINGS: &str =
        "luna://com.webos.settingsservice/setSystemSettings";
}

/// Default plain WebSocket (`ws://`) port, which older firmware still serves.
pub(crate) const WS_PORT: u16 = 3000;
/// Default secure WebSocket (`wss://`) port, which newer firmware requires.
pub(crate) const WSS_PORT: u16 = 3001;

/// Default timeouts in milliseconds.
pub(crate) mod timeouts {
    /// Connection timeout.
    pub(crate) const CONNECT_MS: u64 = 5000;
    /// Request/response timeout.
    pub(crate) const REQUEST_MS: u64 = 10000;
    /// Pairing timeout (user must accept on TV).
    pub(crate) const PAIRING_MS: u64 = 60000;
}
