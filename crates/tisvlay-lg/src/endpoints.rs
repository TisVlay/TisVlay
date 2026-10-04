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

/// Button names accepted by the TV's pointer input socket (`button` messages).
pub mod buttons {
    /// Directional pad: left.
    pub const LEFT: &str = "LEFT";
    /// Directional pad: right.
    pub const RIGHT: &str = "RIGHT";
    /// Directional pad: up.
    pub const UP: &str = "UP";
    /// Directional pad: down.
    pub const DOWN: &str = "DOWN";
    /// Directional pad: OK / select.
    pub const ENTER: &str = "ENTER";
    /// Back out of the current screen.
    pub const BACK: &str = "BACK";
    /// Open the home launcher.
    pub const HOME: &str = "HOME";
    /// Leave the current app or overlay.
    pub const EXIT: &str = "EXIT";
    /// Volume up one step.
    pub const VOLUME_UP: &str = "VOLUMEUP";
    /// Volume down one step.
    pub const VOLUME_DOWN: &str = "VOLUMEDOWN";
    /// Toggle mute.
    pub const MUTE: &str = "MUTE";
    /// Next channel.
    pub const CHANNEL_UP: &str = "CHANNELUP";
    /// Previous channel.
    pub const CHANNEL_DOWN: &str = "CHANNELDOWN";
    /// Media transport: play.
    pub const PLAY: &str = "PLAY";
    /// Media transport: pause.
    pub const PAUSE: &str = "PAUSE";
    /// Media transport: stop.
    pub const STOP: &str = "STOP";
    /// Media transport: rewind.
    pub const REWIND: &str = "REWIND";
    /// Media transport: fast-forward.
    pub const FAST_FORWARD: &str = "FASTFORWARD";
    /// Power key: turns the TV off; waking it back up needs Wake-on-LAN.
    pub const POWER: &str = "POWER";
    /// Color key: red (its meaning depends on the app).
    pub const RED: &str = "RED";
    /// Color key: green (its meaning depends on the app).
    pub const GREEN: &str = "GREEN";
    /// Color key: yellow (its meaning depends on the app).
    pub const YELLOW: &str = "YELLOW";
    /// Color key: blue (its meaning depends on the app).
    pub const BLUE: &str = "BLUE";
    /// Number key 0.
    pub const NUM_0: &str = "0";
    /// Number key 1.
    pub const NUM_1: &str = "1";
    /// Number key 2.
    pub const NUM_2: &str = "2";
    /// Number key 3.
    pub const NUM_3: &str = "3";
    /// Number key 4.
    pub const NUM_4: &str = "4";
    /// Number key 5.
    pub const NUM_5: &str = "5";
    /// Number key 6.
    pub const NUM_6: &str = "6";
    /// Number key 7.
    pub const NUM_7: &str = "7";
    /// Number key 8.
    pub const NUM_8: &str = "8";
    /// Number key 9.
    pub const NUM_9: &str = "9";
    /// The `*` key.
    pub const ASTERISK: &str = "ASTERISK";
    /// Toggle closed captions.
    pub const CC: &str = "CC";
    /// Show the program / info overlay.
    pub const INFO: &str = "INFO";
}

/// Default plain WebSocket (`ws://`) port, which older firmware still serves.
pub const WS_PORT: u16 = 3000;
/// Default secure WebSocket (`wss://`) port, which newer firmware requires.
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
