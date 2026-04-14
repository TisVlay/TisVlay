//! SSAP API endpoint constants.
//!
//! These endpoints are used to control LG WebOS TVs via the SSAP protocol.

/// Power control endpoints.
pub mod power {
    /// Turn off the TV (standby).
    pub const TURN_OFF: &str = "ssap://system/turnOff";

    /// Turn on the TV (requires prior WoL).
    pub const TURN_ON: &str = "ssap://system/turnOn";

    /// Get current power state.
    pub const GET_POWER_STATE: &str = "ssap://com.webos.service.tvpower/power/getPowerState";

    /// Turn off the screen (OLED-safe blank, TV stays on).
    pub const TURN_OFF_SCREEN: &str = "ssap://com.webos.service.tvpower/power/turnOffScreen";

    /// Turn on the screen (resume from blank).
    pub const TURN_ON_SCREEN: &str = "ssap://com.webos.service.tvpower/power/turnOnScreen";
}

/// Input/source control endpoints.
pub mod input {
    /// Switch to a specific input.
    /// Payload: `{"inputId": "HDMI_1"}`
    pub const SWITCH_INPUT: &str = "ssap://tv/switchInput";

    /// Get list of available external inputs.
    pub const GET_EXTERNAL_INPUT_LIST: &str = "ssap://tv/getExternalInputList";

    /// Get the current input.
    pub const GET_CURRENT_INPUT: &str = "ssap://com.webos.applicationManager/getForegroundAppInfo";
}

/// Audio control endpoints.
pub mod audio {
    /// Set volume level.
    /// Payload: `{"volume": 50}`
    pub const SET_VOLUME: &str = "ssap://audio/setVolume";

    /// Get current volume.
    pub const GET_VOLUME: &str = "ssap://audio/getVolume";

    /// Set mute state.
    /// Payload: `{"mute": true}`
    pub const SET_MUTE: &str = "ssap://audio/setMute";

    /// Get mute state.
    pub const GET_MUTE: &str = "ssap://audio/getMute";

    /// Increase volume by one step.
    pub const VOLUME_UP: &str = "ssap://audio/volumeUp";

    /// Decrease volume by one step.
    pub const VOLUME_DOWN: &str = "ssap://audio/volumeDown";

    /// Get sound output mode.
    pub const GET_SOUND_OUTPUT: &str = "ssap://audio/getSoundOutput";

    /// Set sound output mode.
    /// Payload: `{"output": "tv_speaker"}` or `"external_arc"`
    pub const SET_SOUND_OUTPUT: &str = "ssap://audio/changeSoundOutput";
}

/// Picture/display settings endpoints.
pub mod picture {
    /// Get system settings by category.
    /// Payload: `{"category": "picture", "keys": ["contrast", "backlight", "brightness", "color"]}`
    pub const GET_SYSTEM_SETTINGS: &str = "ssap://settings/getSystemSettings";

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

/// Button names for remote input socket.
pub mod buttons {
    pub const LEFT: &str = "LEFT";
    pub const RIGHT: &str = "RIGHT";
    pub const UP: &str = "UP";
    pub const DOWN: &str = "DOWN";
    pub const ENTER: &str = "ENTER";
    pub const BACK: &str = "BACK";
    pub const HOME: &str = "HOME";
    pub const EXIT: &str = "EXIT";
    pub const VOLUME_UP: &str = "VOLUMEUP";
    pub const VOLUME_DOWN: &str = "VOLUMEDOWN";
    pub const MUTE: &str = "MUTE";
    pub const CHANNEL_UP: &str = "CHANNELUP";
    pub const CHANNEL_DOWN: &str = "CHANNELDOWN";
    pub const PLAY: &str = "PLAY";
    pub const PAUSE: &str = "PAUSE";
    pub const STOP: &str = "STOP";
    pub const REWIND: &str = "REWIND";
    pub const FAST_FORWARD: &str = "FASTFORWARD";
    pub const POWER: &str = "POWER";
    pub const RED: &str = "RED";
    pub const GREEN: &str = "GREEN";
    pub const YELLOW: &str = "YELLOW";
    pub const BLUE: &str = "BLUE";
    pub const NUM_0: &str = "0";
    pub const NUM_1: &str = "1";
    pub const NUM_2: &str = "2";
    pub const NUM_3: &str = "3";
    pub const NUM_4: &str = "4";
    pub const NUM_5: &str = "5";
    pub const NUM_6: &str = "6";
    pub const NUM_7: &str = "7";
    pub const NUM_8: &str = "8";
    pub const NUM_9: &str = "9";
    pub const ASTERISK: &str = "ASTERISK";
    pub const CC: &str = "CC";
    pub const INFO: &str = "INFO";
}

/// Default WebSocket ports.
pub const WS_PORT: u16 = 3000;
pub const WSS_PORT: u16 = 3001;

/// Default timeouts in milliseconds.
pub mod timeouts {
    /// Connection timeout.
    pub const CONNECT_MS: u64 = 5000;
    /// Request/response timeout.
    pub const REQUEST_MS: u64 = 10000;
    /// Pairing timeout (user must accept on TV).
    pub const PAIRING_MS: u64 = 60000;
    /// Delay after WoL before sending commands.
    pub const WOL_DELAY_MS: u64 = 3000;
}
