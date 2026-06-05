//! Epson projector vendor configuration.
//!
//! Stored in the `[tv.epson]` section of the daemon config. Only `ip`
//! is required — the projector speaks unauthenticated ESC/VP21 over
//! TCP, so there is no credential to persist.

use serde::{Deserialize, Serialize};
use tisvlay_core::{Error, Result};

/// Default ESC/VP21 TCP port.
const DEFAULT_PORT: u16 = 3629;

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// Epson projector vendor-specific configuration.
///
/// # Example
///
/// ```toml
/// [[tv]]
/// id = "theater"
/// name = "Living Room Projector"
/// vendor = "epson"
///
/// [tv.epson]
/// ip = "192.168.1.50"
/// mac = "AA:BB:CC:DD:EE:FF"
/// model = "Pro Cinema LS12000"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpsonVendorConfig {
    /// IP address of the projector's ESC/VP21 TCP interface.
    pub ip: String,

    /// MAC address for Wake-on-LAN (optional).
    ///
    /// When present, the backend can wake the projector from deep
    /// standby with a magic packet; without it, only the network
    /// `PWR ON` path (which needs the projector already on the LAN)
    /// is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,

    /// Model string, manually entered for display in diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// ESC/VP21 TCP port. Defaults to 3629; overridable so tests can
    /// point the client at a fake server on an ephemeral port. Not
    /// surfaced in the config schema — there is no second transport
    /// yet that would justify a user-facing port selector.
    #[serde(default = "default_port")]
    pub port: u16,
}

impl EpsonVendorConfig {
    /// Create a config with the required IP, default port, and no
    /// MAC or model.
    pub fn new(ip: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            mac: None,
            model: None,
            port: DEFAULT_PORT,
        }
    }

    /// Create a config with IP and MAC for Wake-on-LAN.
    pub fn with_mac(ip: impl Into<String>, mac: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            mac: Some(mac.into()),
            model: None,
            port: DEFAULT_PORT,
        }
    }

    /// Parse an [`EpsonVendorConfig`] from a generic `toml::Value`.
    pub fn from_toml(value: &toml::Value) -> Result<Self> {
        value
            .clone()
            .try_into()
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("invalid epson config: {e}")))
    }

    /// Serialize this config back to a generic `toml::Value`.
    pub fn to_toml(&self) -> Result<toml::Value> {
        toml::Value::try_from(self)
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("serialize epson config: {e}")))
    }
}

/// Full persisted-config schema for the Epson backend.
///
/// Visibility tags drive projection: the pairing wizard and INFO pane
/// see `UserInput` (`ip`, `mac`); the INFO pane also shows
/// `Diagnostic` (`model`). The `ip` row is first so it becomes the
/// backend's primary descriptor.
pub fn config_schema() -> Vec<tisvlay_core::PairingField> {
    use tisvlay_core::{FieldVisibility, PairingField, PairingFieldType};
    vec![
        PairingField {
            name: "ip".into(),
            label: "IP".into(),
            helper: "LAN address of the projector's ESC/VP21 interface.".into(),
            field_type: PairingFieldType::Ip,
            required: true,
            placeholder: "192.168.1.100".into(),
            // No-op until Epson discovery lands; matches the
            // `discovery.<key>` autofill convention.
            autofill: "discovery.ip".into(),
            ..Default::default()
        },
        PairingField {
            name: "mac".into(),
            label: "MAC".into(),
            helper: "Required for Wake-on-LAN — without it the daemon \
                     can't power this projector on from deep standby."
                .into(),
            field_type: PairingFieldType::Mac,
            required: false,
            placeholder: "AA:BB:CC:DD:EE:FF".into(),
            autofill: "discovery.mac".into(),
            ..Default::default()
        },
        PairingField {
            name: "model".into(),
            label: "Model".into(),
            helper: "Optional model label shown in diagnostics.".into(),
            field_type: PairingFieldType::String,
            // Read-only INFO row — surfaced for diagnosis, not editable
            // in the pairing form.
            visibility: FieldVisibility::Diagnostic,
            ..Default::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::FieldVisibility;

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
            ip = "192.168.1.50"
            mac = "AA:BB:CC:DD:EE:FF"
            model = "Pro Cinema LS12000"
        "#;
        let config: EpsonVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ip, "192.168.1.50");
        assert_eq!(config.mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(config.model.as_deref(), Some("Pro Cinema LS12000"));
        // Port defaults when absent.
        assert_eq!(config.port, 3629);
    }

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"ip = "192.168.1.50""#;
        let config: EpsonVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.ip, "192.168.1.50");
        assert!(config.mac.is_none());
        assert!(config.model.is_none());
        assert_eq!(config.port, 3629);
    }

    #[test]
    fn parse_honors_port_override() {
        let toml_str = r#"
            ip = "127.0.0.1"
            port = 40000
        "#;
        let config: EpsonVendorConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 40000);
    }

    #[test]
    fn parse_rejects_missing_ip() {
        let toml_str = r#"mac = "AA:BB:CC:DD:EE:FF""#;
        let result: std::result::Result<EpsonVendorConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err(), "ip is required");
    }

    #[test]
    fn from_toml_wraps_error_as_backend() {
        // A non-table value can't deserialize into the struct.
        let value = toml::Value::String("nope".into());
        let err = EpsonVendorConfig::from_toml(&value).unwrap_err();
        assert!(err.to_string().contains("epson"));
    }

    #[test]
    fn to_toml_round_trips() {
        let config = EpsonVendorConfig::with_mac("10.0.0.1", "AA:BB:CC:DD:EE:FF");
        let value = config.to_toml().unwrap();
        let parsed = EpsonVendorConfig::from_toml(&value).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn serialize_omits_none_fields() {
        let config = EpsonVendorConfig::new("10.0.0.1");
        let toml_str = toml::to_string(&config).unwrap();
        assert!(!toml_str.contains("mac"));
        assert!(!toml_str.contains("model"));
        // Port is always serialized (it has a value, not an Option).
        assert!(toml_str.contains("port"));
    }

    #[test]
    fn schema_orders_ip_first_and_tags_visibility() {
        let schema = config_schema();
        assert_eq!(schema[0].name, "ip");
        assert!(schema[0].required);
        assert_eq!(schema[0].visibility, FieldVisibility::UserInput);

        let mac = schema.iter().find(|f| f.name == "mac").unwrap();
        assert!(!mac.required);
        assert_eq!(mac.visibility, FieldVisibility::UserInput);

        let model = schema.iter().find(|f| f.name == "model").unwrap();
        assert_eq!(model.visibility, FieldVisibility::Diagnostic);

        // No user-facing port/endpoint row.
        assert!(schema.iter().all(|f| f.name != "port"));
        assert!(schema.iter().all(|f| f.name != "endpoint"));
    }
}
