//! LG WebOS specific configuration.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tisvlay_core::Error;

/// WebSocket scheme an LG TV speaks.
///
/// `Wss` (port 3001 + TLS with self-signed cert) is the modern
/// firmware default; `Ws` (port 3000, plain) is older firmware that
/// shipped before LG's TLS rollout. Pairing probes both via
/// [`Connection::connect`](crate::transport::Connection::connect)
/// and persists the working one into [`LgVendorConfig::scheme`] so
/// post-pairing reconnects dispatch directly without the fallback
/// round-trip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebOsScheme {
    /// Secure WebSocket on port 3001 (TLS with self-signed cert).
    /// Default for modern firmware.
    #[default]
    Wss,
    /// Plain WebSocket on port 3000. Older firmware only.
    Ws,
}

impl WebOsScheme {
    /// Stable lowercase identifier — matches the serde rename and
    /// the URL scheme component (`"wss"` / `"ws"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wss => "wss",
            Self::Ws => "ws",
        }
    }
}

impl std::fmt::Display for WebOsScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WebOsScheme {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "wss" => Ok(Self::Wss),
            "ws" => Ok(Self::Ws),
            other => Err(Error::backend(
                "lg",
                format!("Unknown WebSocket scheme: {other}"),
            )),
        }
    }
}

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
/// id = "living_room"
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
    ///
    /// Also accepts `"address"` as a generic alias so the CLI can pass
    /// a vendor-agnostic field name during pairing.
    #[serde(alias = "address")]
    pub ip: String,

    /// MAC address for Wake-on-LAN (optional).
    ///
    /// Also accepts `"wake_id"` as a generic alias so the CLI can pass
    /// a vendor-agnostic field name during pairing.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "wake_id")]
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

    /// WebSocket scheme this TV speaks. Captured at pair-time and
    /// persisted so reconnects dispatch directly to the right
    /// scheme without probing — saves a TLS handshake per connect
    /// for ws-only TVs and skips a wss→ws fallback round-trip for
    /// wss-capable ones. Defaults to `WebOsScheme::Wss` when
    /// absent so configs that pre-date this field deserialize
    /// without surprise (re-pair if your TV is `ws`-only).
    #[serde(default)]
    pub scheme: WebOsScheme,
}

impl LgVendorConfig {
    /// Create a new config with the required IP address.
    pub fn new(ip: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            mac: None,
            client_key: None,
            default_input: None,
            scheme: WebOsScheme::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
            ip = "192.168.1.100"
            mac = "AA:BB:CC:DD:EE:FF"
            client_key = "abc123"
            default_input = "HDMI_1"
        "#;
        let config: LgVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ip, "192.168.1.100");
        assert_eq!(config.mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(config.client_key.as_deref(), Some("abc123"));
        assert_eq!(config.default_input.as_deref(), Some("HDMI_1"));
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"ip = "192.168.1.100""#;
        let config: LgVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ip, "192.168.1.100");
        assert!(config.mac.is_none());
        assert!(config.client_key.is_none());
        assert!(config.default_input.is_none());
    }

    #[test]
    fn address_alias_maps_to_ip() {
        let toml_str = r#"address = "192.168.1.200""#;
        let config: LgVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ip, "192.168.1.200");
    }

    #[test]
    fn wake_id_alias_maps_to_mac() {
        let toml_str = r#"
            ip = "10.0.0.1"
            wake_id = "11:22:33:44:55:66"
        "#;
        let config: LgVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mac.as_deref(), Some("11:22:33:44:55:66"));
    }

    #[test]
    fn serialize_round_trip() {
        let config = LgVendorConfig {
            ip: "10.0.0.1".to_string(),
            mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
            client_key: Some("key123".to_string()),
            default_input: Some("HDMI_2".to_string()),
            scheme: WebOsScheme::Ws,
        };
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: LgVendorConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn serialize_omits_none_fields() {
        let config = LgVendorConfig::new("10.0.0.1");
        let toml_str = toml::to_string(&config).unwrap();
        assert!(!toml_str.contains("mac"));
        assert!(!toml_str.contains("client_key"));
        assert!(!toml_str.contains("default_input"));
    }

    #[test]
    fn scheme_defaults_when_absent() {
        let toml_str = r#"
            ip = "192.168.1.100"
            client_key = "abc123"
        "#;
        let config: LgVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.scheme, WebOsScheme::Wss);
    }

    #[test]
    fn builder_methods() {
        let mut config = LgVendorConfig::new("10.0.0.1");
        config.mac = Some("AA:BB:CC:DD:EE:FF".into());
        config.set_client_key("key");
        config.set_default_input("HDMI_3");
        assert_eq!(config.ip, "10.0.0.1");
        assert_eq!(config.mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(config.client_key.as_deref(), Some("key"));
        assert_eq!(config.default_input.as_deref(), Some("HDMI_3"));
    }
}
