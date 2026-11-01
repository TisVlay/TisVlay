//! Vendor-agnostic pairing form schema.
//!
//! Each vendor backend declares (via `TvConnector::pairing_schema`)
//! the form fields its pairing flow needs from the user. The daemon
//! exposes the schema over D-Bus as JSON; the GUI renders the form
//! dynamically — no backend-specific UI code lives in the wizard.
//!
//! Modeled loosely on Home Assistant's [config_flow][1] schema and
//! NetworkManager's per-connection-type setting groups: one schema
//! per backend, generic UI consumer, server-side canonical
//! validation.
//!
//! Scope is the *pairing* form only (fields the user enters
//! BEFORE pairing). A separate `GetConfigSchema` for editing
//! already-paired TVs is on the roadmap.
//!
//! [1]: https://developers.home-assistant.io/docs/data_entry_flow_index/
//!
//! # Example
//!
//! An LG WebOS backend's pairing schema declares two fields:
//!
//! ```ignore
//! vec![
//!     PairingField {
//!         name: "ip".into(),
//!         label: "IP".into(),
//!         field_type: PairingFieldType::Ip,
//!         required: true,
//!         autofill: "discovery.address".into(),
//!         ..Default::default()
//!     },
//!     PairingField {
//!         name: "mac".into(),
//!         label: "MAC".into(),
//!         helper: "Required for Wake-on-LAN.".into(),
//!         field_type: PairingFieldType::Mac,
//!         required: false,
//!         autofill: "discovery.mac".into(),
//!         ..Default::default()
//!     },
//! ]
//! ```

use serde::{Deserialize, Serialize};

/// How a vendor-config field flows through the system.
///
/// One axis of metadata that lets the framework decide what's safe
/// to do with each field: include in the pairing form, surface in
/// diagnostics, redact entirely. Backends mark each field once on
/// the schema; the framework projects the right view per use case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldVisibility {
    /// User types it during pairing (`ip`, `mac`). Shown in the
    /// pairing wizard, surfaced in the INFO pane, editable in the
    /// future edit-properties pane. The default — backends without
    /// a reason to mark otherwise get this implicitly.
    #[default]
    UserInput,
    /// Backend-discovered during pairing or connection
    /// (`scheme` for LG, hypothetical Samsung capability strings).
    /// Surfaced in the INFO pane for diagnosis but never editable
    /// — re-pair to refresh.
    Diagnostic,
    /// Sensitive backend-discovered value (`client_key`, OAuth
    /// tokens, PSKs). Never displayed, never sent to the GUI's
    /// `display_info` projection. The default `display_info`
    /// implementation skips these unconditionally.
    Secret,
}

/// Input type for a [`PairingField`]. The wizard picks the right
/// Slint widget based on this; the daemon's submit-validation also
/// dispatches on it for canonical format checks (IP / MAC / port
/// range).
///
/// Add a variant when a new backend genuinely needs it; don't
/// reuse `String` as a catch-all when a typed variant fits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairingFieldType {
    /// Plain single-line text. Default for fields that don't
    /// specify a type explicitly.
    #[default]
    String,
    /// Masked text (passwords, vendor API keys, PSKs).
    Password,
    /// IPv4 or IPv6 address.
    Ip,
    /// MAC address (`XX:XX:XX:XX:XX:XX` or `XX-XX-XX-XX-XX-XX`).
    Mac,
    /// TCP/UDP port (1..=65535).
    Port,
    /// Generic signed integer.
    Int,
    /// Checkbox / toggle.
    Bool,
    /// Dropdown — `options` carries the choices.
    Select,
    /// Filesystem path (e.g. serial port `/dev/ttyUSB0`).
    Path,
}

impl PairingFieldType {
    /// Wire-format kebab-case identifier (`"mac"`, `"ip"`,
    /// `"select"`, …) — the same string `serde` emits for this
    /// variant. Useful for sending to non-Rust UI layers (Slint
    /// can't read serde tags directly).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Password => "password",
            Self::Ip => "ip",
            Self::Mac => "mac",
            Self::Port => "port",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::Select => "select",
            Self::Path => "path",
        }
    }
}

/// Single field in a pairing form.
///
/// Fields default to `required: false`, `field_type: String`, and
/// no autofill. A bare `PairingField { name, label,
/// ..Default::default() }` is a valid optional plain-text field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairingField {
    /// Internal identifier. Used as:
    /// - The JSON key in the pairing-config payload sent back on
    ///   the `add-tv` / `pair` rpcs.
    /// - The `field` token in
    ///   [`ConfigError::InvalidVendorConfig`][1] so the GUI can
    ///   route validation errors back to the originating widget.
    /// - The autofill match key (`autofill: "discovery.address"`
    ///   matches `vendor_data["address"]`).
    ///
    /// [1]: ../errors/config/enum.ConfigError.html
    pub name: String,

    /// User-facing label (English source). When `translation_key`
    /// is set, the GUI routes this through gettext for display.
    pub label: String,

    /// Optional helper / explanation text shown under the field.
    /// Same translation pathway as `label`.
    #[serde(default)]
    pub helper: String,

    /// Optional gettext key for `label` + `helper` lookup. `None`
    /// = use the English literals. Per-vendor `.mo` catalogs keyed
    /// off these IDs are the planned home for vendor strings
    /// (`docs/i18n.md` in the repository).
    #[serde(default)]
    pub translation_key: Option<String>,

    /// Input type — drives both UI rendering and server-side
    /// canonical validation.
    #[serde(default)]
    pub field_type: PairingFieldType,

    /// Whether the user must provide a value before the wizard
    /// can submit.
    #[serde(default)]
    pub required: bool,

    /// Placeholder text shown when the field is empty.
    #[serde(default)]
    pub placeholder: String,

    /// Optional default value the form pre-fills with.
    #[serde(default)]
    pub default: String,

    /// Optional autofill source key.
    ///
    /// Format: `discovery.<key>` — when discovery yielded
    /// `vendor_data[<key>]` for the chosen TV, the wizard
    /// pre-fills this field with that value (still editable).
    /// Empty string = no autofill.
    #[serde(default)]
    pub autofill: String,

    /// Choices for `field_type: Select`. Ignored for other types.
    #[serde(default)]
    pub options: Vec<String>,

    /// Optional client-side validation regex. The daemon also
    /// validates server-side; the regex is just for early
    /// feedback in the form-validation lifecycle.
    #[serde(default)]
    pub validation: Option<String>,

    /// Where the value comes from + whether it's safe to display.
    /// Drives projection: pairing wizard shows `UserInput` only,
    /// `display_info` shows `UserInput + Diagnostic`, future edit
    /// pane edits `UserInput`. Defaults to `UserInput` so existing
    /// pairing-form fields don't need to opt in.
    #[serde(default)]
    pub visibility: FieldVisibility,
}

impl PairingField {
    /// Server-side canonical validation of a submitted value.
    ///
    /// Checks `required` + the type-specific format (IP / MAC /
    /// port-range / select-options). Empty `value` means
    /// "user left the field blank"; the JSON-on-the-wire shape
    /// (the `add-tv` rpc's `config` object) typically simply omits
    /// optional fields, so callers should pass `""` for absent
    /// values.
    ///
    /// Returns `Ok(())` on success, or a typed
    /// [`InvalidVendorConfig`](crate::errors::ConfigError::InvalidVendorConfig)
    /// carrying this field's `name` and a human-readable reason.
    pub fn validate(&self, value: &str) -> Result<(), crate::errors::ConfigError> {
        let invalid = |reason: &str| crate::errors::ConfigError::InvalidVendorConfig {
            field: self.name.clone(),
            reason: reason.to_string(),
        };

        if value.is_empty() {
            return if self.required {
                Err(invalid("required"))
            } else {
                Ok(())
            };
        }

        match self.field_type {
            // No format constraints beyond non-empty (already checked).
            PairingFieldType::String
            | PairingFieldType::Password
            | PairingFieldType::Path
            | PairingFieldType::Bool
            | PairingFieldType::Int => Ok(()),
            PairingFieldType::Ip => value
                .parse::<std::net::IpAddr>()
                .map(|_| ())
                .map_err(|_| invalid("must be a valid IPv4 or IPv6 address")),
            PairingFieldType::Mac => {
                if is_valid_mac(value) {
                    Ok(())
                } else {
                    Err(invalid(
                        "must be six hex pairs separated by colons or hyphens",
                    ))
                }
            }
            PairingFieldType::Port => value
                .parse::<u16>()
                .map(|p| if p == 0 { Err(()) } else { Ok(()) })
                .map_err(|_| ())
                .and_then(|r| r)
                .map_err(|()| invalid("must be a number between 1 and 65535")),
            PairingFieldType::Select => {
                if self.options.iter().any(|opt| opt == value) {
                    Ok(())
                } else {
                    Err(invalid("must be one of the available options"))
                }
            }
        }
    }
}

/// Format check for `XX:XX:XX:XX:XX:XX` or `XX-XX-XX-XX-XX-XX`.
/// Six hex pairs separated by `:` or `-` (mixed not allowed).
fn is_valid_mac(s: &str) -> bool {
    let sep = if s.contains(':') {
        ':'
    } else if s.contains('-') {
        '-'
    } else {
        return false;
    };
    let parts: Vec<&str> = s.split(sep).collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_type_serializes_as_kebab_case() {
        let json = serde_json::to_string(&PairingFieldType::Mac).unwrap();
        assert_eq!(json, "\"mac\"");
        let json = serde_json::to_string(&PairingFieldType::Select).unwrap();
        assert_eq!(json, "\"select\"");
    }

    #[test]
    fn validate_required_empty_rejects() {
        let f = PairingField {
            name: "ip".into(),
            label: "IP".into(),
            field_type: PairingFieldType::Ip,
            required: true,
            ..Default::default()
        };
        let err = f.validate("").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("ip"));
        assert!(s.contains("required"));
    }

    #[test]
    fn validate_optional_empty_passes() {
        let f = PairingField {
            name: "mac".into(),
            label: "MAC".into(),
            field_type: PairingFieldType::Mac,
            required: false,
            ..Default::default()
        };
        assert!(f.validate("").is_ok());
    }

    #[test]
    fn validate_ip_format() {
        let f = PairingField {
            name: "ip".into(),
            field_type: PairingFieldType::Ip,
            required: true,
            ..Default::default()
        };
        assert!(f.validate("192.168.1.100").is_ok());
        assert!(f.validate("::1").is_ok());
        assert!(f.validate("not.an.ip").is_err());
    }

    #[test]
    fn validate_mac_format() {
        let f = PairingField {
            name: "mac".into(),
            field_type: PairingFieldType::Mac,
            required: true,
            ..Default::default()
        };
        assert!(f.validate("AA:BB:CC:DD:EE:FF").is_ok());
        assert!(f.validate("aa-bb-cc-dd-ee-ff").is_ok());
        assert!(f.validate("AA:BB:CC:DD:EE").is_err()); // five pairs
        assert!(f.validate("ZZ:BB:CC:DD:EE:FF").is_err()); // non-hex
    }

    #[test]
    fn validate_port_range() {
        let f = PairingField {
            name: "port".into(),
            field_type: PairingFieldType::Port,
            required: true,
            ..Default::default()
        };
        assert!(f.validate("3001").is_ok());
        assert!(f.validate("65535").is_ok());
        assert!(f.validate("0").is_err());
        assert!(f.validate("70000").is_err());
        assert!(f.validate("hello").is_err());
    }

    #[test]
    fn validate_select_options() {
        let f = PairingField {
            name: "color".into(),
            field_type: PairingFieldType::Select,
            required: true,
            options: vec!["red".into(), "green".into(), "blue".into()],
            ..Default::default()
        };
        assert!(f.validate("green").is_ok());
        assert!(f.validate("yellow").is_err());
    }

    #[test]
    fn field_default_round_trips() {
        let f = PairingField {
            name: "ip".into(),
            label: "IP".into(),
            field_type: PairingFieldType::Ip,
            required: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: PairingField = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "ip");
        assert_eq!(back.field_type, PairingFieldType::Ip);
        assert!(back.required);
        // Defaults preserved through round-trip.
        assert_eq!(back.helper, "");
        assert!(back.options.is_empty());
        assert_eq!(back.translation_key, None);
    }
}
