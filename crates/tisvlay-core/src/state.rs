//! TV state model.

use serde::{Deserialize, Serialize};

/// Current state of a TV.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TvState {
    /// Whether the TV is powered on.
    pub is_on: bool,
    /// Whether the screen is on (false = blanked for OLED protection).
    pub is_screen_on: bool,
    /// Current volume level (0-100).
    pub volume: Option<i32>,
    /// Whether audio is muted.
    pub muted: Option<bool>,
    /// Current input ID (e.g., "HDMI_1").
    pub current_input: Option<String>,
    /// Current backlight level (0-100, OLED pixel brightness).
    pub backlight: Option<i32>,
    /// Current foreground app ID.
    pub current_app: Option<String>,
    /// Available inputs.
    pub inputs: Vec<InputInfo>,
}

/// Information about an input source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputInfo {
    /// Input ID (e.g., "HDMI_1", "HDMI_2").
    pub id: String,
    /// Display label (e.g., "HDMI 1", "PC").
    pub label: String,
    /// Whether a device is connected to this input.
    pub connected: bool,
}

/// TV power state as reported by the TV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    /// TV is fully on.
    Active,
    /// TV is in standby.
    Standby,
    /// TV is powering on.
    ActiveStandby,
    /// TV is in suspend mode.
    Suspend,
    /// TV is in screen saver mode.
    ScreenSaver,
    /// TV is in screen off mode (OLED blank).
    ScreenOff,
    /// Unknown state.
    Unknown,
}

impl PowerState {
    /// Parse power state from TV response string.
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "active" => Self::Active,
            "standby" | "suspend" => Self::Standby,
            "active standby" | "activestandby" => Self::ActiveStandby,
            "screen saver" | "screensaver" => Self::ScreenSaver,
            "screen off" | "screenoff" => Self::ScreenOff,
            _ => Self::Unknown,
        }
    }

    /// Whether the TV is considered "on" (not in standby).
    pub fn is_on(self) -> bool {
        matches!(
            self,
            Self::Active | Self::ActiveStandby | Self::ScreenSaver | Self::ScreenOff
        )
    }
}

impl TvState {
    /// Create a new empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update state from power state response.
    pub fn update_power(&mut self, state: PowerState) {
        self.is_on = state.is_on();
        self.is_screen_on = !matches!(state, PowerState::ScreenOff | PowerState::Standby);
    }
}
