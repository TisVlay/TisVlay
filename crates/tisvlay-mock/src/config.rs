//! Vendor config for the mock backend.

use serde::{Deserialize, Serialize};

/// Mock TV configuration, stored under `[tv.mock]` in the daemon config.
///
/// Only `address` is required — everything else has sensible defaults.
/// The address is purely a label shown in logs and the tray; no actual
/// network traffic happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockVendorConfig {
    /// Virtual address for the mock TV (e.g., `"mock://living_room"`
    /// or `"192.168.255.1"`). Shown in logs and the tray tooltip.
    #[serde(default = "default_address")]
    pub address: String,

    /// Initial power state: one of `"active"`, `"standby"`, `"screen_off"`.
    #[serde(default = "default_power_state")]
    pub initial_power_state: String,

    /// Initial volume (0-100).
    #[serde(default = "default_volume")]
    pub initial_volume: u8,

    /// Initial input ID (must exist in `inputs`).
    #[serde(default = "default_input")]
    pub initial_input: String,

    /// Initial backlight level (0-100).
    #[serde(default = "default_backlight")]
    pub initial_backlight: u8,

    /// Available inputs as `(id, label)` pairs.
    #[serde(default = "default_inputs")]
    pub inputs: Vec<(String, String)>,
}

fn default_address() -> String {
    "mock://tv".to_string()
}

fn default_power_state() -> String {
    "active".to_string()
}

fn default_volume() -> u8 {
    30
}

fn default_input() -> String {
    "HDMI_1".to_string()
}

fn default_backlight() -> u8 {
    80
}

fn default_inputs() -> Vec<(String, String)> {
    vec![
        ("HDMI_1".to_string(), "HDMI 1".to_string()),
        ("HDMI_2".to_string(), "HDMI 2".to_string()),
        ("HDMI_3".to_string(), "HDMI 3".to_string()),
    ]
}

impl Default for MockVendorConfig {
    fn default() -> Self {
        Self {
            address: default_address(),
            initial_power_state: default_power_state(),
            initial_volume: default_volume(),
            initial_input: default_input(),
            initial_backlight: default_backlight(),
            inputs: default_inputs(),
        }
    }
}
