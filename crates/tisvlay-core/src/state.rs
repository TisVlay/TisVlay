//! TV state model.

use crate::HardwareId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Current state of a TV.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TvState {
    /// Whether the TV is powered on.
    pub is_on: bool,
    /// Whether the screen is on (false = blanked for OLED protection).
    pub is_screen_on: bool,
    /// Current volume level (0-100).
    pub volume: Option<u8>,
    /// Whether audio is muted.
    pub muted: Option<bool>,
    /// Current input ID, normalized to physical ID space (e.g., "HDMI_1").
    pub current_input: Option<String>,
    /// Current backlight level (0-100, OLED pixel brightness).
    pub backlight: Option<u8>,
    /// Current foreground app ID.
    pub current_app: Option<String>,
    /// Available inputs.
    pub inputs: Vec<InputInfo>,
    /// Vendor-internal mapping from app IDs to physical input IDs.
    ///
    /// Populated by backends that use app-based input identification
    /// (e.g., LG maps `"com.webos.app.hdmi1"` → `"HDMI_1"`). Used to
    /// normalize `current_input` to the physical ID space so it matches
    /// the `inputs` list. Skipped during serialization (internal only).
    #[serde(skip)]
    pub input_app_id_map: HashMap<String, String>,
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
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "active" => Self::Active,
            "standby" => Self::Standby,
            "suspend" => Self::Suspend,
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
        self.is_screen_on = !matches!(
            state,
            PowerState::ScreenOff | PowerState::Standby | PowerState::Suspend
        );
    }
}

/// Information about a discovered TV on the network.
///
/// This is returned by discovery mechanisms (SSDP, mDNS, etc.) and contains
/// enough information to identify and connect to the TV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredTv {
    /// Unique identifier (e.g., UUID from SSDP USN header).
    pub id: String,
    /// TV friendly name.
    pub name: String,
    /// Transport address for display (e.g., IP address, serial port, CEC address).
    pub address: String,
    /// Hardware identity (manufacturer / model / serial) as reported by
    /// the discovery protocol. Backends fill what they can read; may be
    /// `None` if the discovery channel exposes no identity data.
    #[serde(default)]
    pub hardware_id: Option<HardwareId>,
    /// Vendor identifier (e.g., "lg", "samsung").
    pub vendor: String,
    /// Location URL for device description (vendor-specific).
    #[serde(default)]
    pub location: String,
}

impl DiscoveredTv {
    /// Create a new discovered TV entry.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        address: impl Into<String>,
        vendor: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            hardware_id: None,
            vendor: vendor.into(),
            location: String::new(),
        }
    }

    /// Attach a hardware identity to this discovery entry.
    pub fn with_hardware_id(mut self, hardware_id: HardwareId) -> Self {
        self.hardware_id = Some(hardware_id);
        self
    }

    /// Set the location URL.
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = location.into();
        self
    }
}
