//! LG WebOS specific configuration.

use serde::{Deserialize, Serialize};

/// LG WebOS vendor-specific configuration.
///
/// This contains all settings specific to LG WebOS TVs, stored in the
/// `[tv.lg]` section of the config file. Use `TvConfig::get_vendor_config()`
/// to deserialize this from the generic vendor config map.
///
/// # Example
///
/// ```toml
/// [[tv]]
/// id = "living-room"
/// name = "Living Room TV"
/// vendor = "lg"
///
/// [tv.lg]
/// ip = "192.168.1.100"
/// mac = "AA:BB:CC:DD:EE:FF"
/// client_key = "abc123"
/// default_input = "HDMI_1"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LgVendorConfig {
    /// IP address for WebSocket connection.
    pub ip: String,

    /// MAC address for Wake-on-LAN (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,

    /// Client key obtained during pairing.
    ///
    /// This is a unique key that the TV generates when you accept the
    /// pairing prompt. It's used for subsequent connections without
    /// requiring user interaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,

    /// Default HDMI input to switch to on wake.
    ///
    /// When specified, the TV will switch to this input after being
    /// powered on via Wake-on-LAN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_input: Option<String>,
}

impl LgVendorConfig {
    /// Create a new config with the required IP address.
    pub fn new(ip: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            mac: None,
            client_key: None,
            default_input: None,
        }
    }

    /// Create a config with IP and MAC for Wake-on-LAN.
    pub fn with_mac(ip: impl Into<String>, mac: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            mac: Some(mac.into()),
            client_key: None,
            default_input: None,
        }
    }

    /// Set the client key.
    pub fn set_client_key(&mut self, key: impl Into<String>) {
        self.client_key = Some(key.into());
    }

    /// Set the default input.
    pub fn set_default_input(&mut self, input: impl Into<String>) {
        self.default_input = Some(input.into());
    }
}

/// Type alias for backwards compatibility within this crate.
pub type LgConfig = LgVendorConfig;
