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
//! Per [plan5.md][2] phase 1. v1 schema scope is the *pairing* form
//! only (what the user enters BEFORE pairing); a separate
//! `GetConfigSchema` for editing existing TVs lands with
//! the edit-properties work.
//!
//! [1]: https://developers.home-assistant.io/docs/data_entry_flow_index/
//! [2]: ../../docs/plan5.md
//!
//! # Example
//!
//! An LG WebOS backend's pairing schema declares two fields:
//!
//! ```ignore
//! vec![
//!     PairingField {
//!         name: "ip".into(),
//!         label: "IP address".into(),
//!         field_type: PairingFieldType::Ip,
//!         required: true,
//!         autofill: "discovery.address".into(),
//!         ..Default::default()
//!     },
//!     PairingField {
//!         name: "mac".into(),
//!         label: "MAC address".into(),
//!         helper: "Required for Wake-on-LAN.".into(),
//!         field_type: PairingFieldType::Mac,
//!         required: false,
//!         autofill: "discovery.mac".into(),
//!         ..Default::default()
//!     },
//! ]
//! ```

use serde::{Deserialize, Serialize};

/// Input type for a [`PairingField`]. The wizard picks the right
/// Slint widget based on this; the daemon's submit-validation also
/// dispatches on it for canonical format checks (IP / MAC / port
/// range).
///
/// Per v3 Q4 of [plan5.md] this is the canonical vocabulary. Add a
/// variant when a new backend genuinely needs it; don't reuse
/// `String` as a catch-all when a typed variant fits.
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

/// Single field in a pairing form.
///
/// Fields default to `required: false`, `field_type: String`, and
/// no autofill. A bare `PairingField { name, label,
/// ..Default::default() }` is a valid optional plain-text field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairingField {
    /// Internal identifier. Used as:
    /// - The JSON key in the pairing-config payload sent back to
    ///   `Manager1.AddTv`.
    /// - The `field` token in
    ///   [`ConfigError::InvalidVendorConfig`][1] so the GUI can
    ///   route validation errors back to the originating widget.
    /// - The autofill match key (`autofill: "discovery.address"`
    ///   matches `vendor_data["address"]`).
    ///
    /// [1]: ../errors/config/enum.ConfigError.html
    pub name: String,

    /// User-facing label (English source). Phase 5 of plan5 will
    /// route this through gettext when `translation_key` is set.
    pub label: String,

    /// Optional helper / explanation text shown under the field.
    /// Same translation pathway as `label`.
    #[serde(default)]
    pub helper: String,

    /// Optional gettext key for `label` + `helper` lookup. `None`
    /// = use the English literals. Vendor crates ship their own
    /// `.mo` catalogs keyed off these IDs (per
    /// [`.claude/CLAUDE.md`][1]'s i18n architecture section).
    ///
    /// [1]: ../../../.claude/CLAUDE.md
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
    fn field_default_round_trips() {
        let f = PairingField {
            name: "ip".into(),
            label: "IP address".into(),
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
