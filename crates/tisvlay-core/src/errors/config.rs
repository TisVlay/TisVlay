//! Typed errors for configuration validation.
//!
//! Surfaces the typed shape of pairing- and config-form
//! validation failures. Errors carry the schema field's `name`
//! identifier (machine-readable) so the GUI can route the message
//! back to the correct widget and render the field's localized
//! label alongside the reason — keeping the daemon
//! locale-agnostic.

/// Configuration validation errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A schema-validated pairing/config field has an invalid or
    /// missing value. `field` carries the schema's `name`
    /// identifier (e.g. `"mac"`), `reason` an English (gettext-
    /// keyable) explanation suitable for inline-error display.
    #[error("invalid value for `{field}`: {reason}")]
    InvalidVendorConfig {
        /// Schema field name (machine-readable; matches
        /// `PairingField::name`). The GUI maps this back to the
        /// localized field label for display.
        field: String,
        /// Human-readable failure reason.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_vendor_config_carries_field_and_reason() {
        let e = ConfigError::InvalidVendorConfig {
            field: "mac".into(),
            reason: "must be six hex pairs".into(),
        };
        let s = e.to_string();
        assert!(s.contains("mac"));
        assert!(s.contains("six hex pairs"));
    }
}
