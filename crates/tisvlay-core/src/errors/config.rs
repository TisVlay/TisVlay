//! Typed errors for configuration validation.
//!
//! Surfaces the typed shape of pairing- and config-form
//! validation failures. Per plan5 phase 1 + v4 Q2: errors carry
//! the schema field's `name` identifier (machine-readable) so the
//! GUI can route the message back to the correct widget and
//! render the field's localized label alongside the reason.

use std::fmt;

/// Stable machine-readable code for a [`ConfigError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigErrorCode {
    /// A schema-validated field has an invalid or missing value.
    InvalidVendorConfig,
    /// Attempted to add a TV whose id collides with an existing one.
    DuplicateTvId,
    /// A TV references a profile that doesn't exist.
    InvalidProfileReference,
}

impl ConfigErrorCode {
    /// Stable identifier for routing on clients (kebab-case under
    /// the `config.` namespace).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidVendorConfig => "config.invalid-vendor-config",
            Self::DuplicateTvId => "config.duplicate-tv-id",
            Self::InvalidProfileReference => "config.invalid-profile-reference",
        }
    }
}

impl fmt::Display for ConfigErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration validation errors.
#[derive(Debug, Clone, thiserror::Error)]
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

    /// A new TV's id collides with an existing one in the daemon
    /// config.
    #[error("TV id `{0}` is already in use")]
    DuplicateTvId(String),

    /// A TV references a profile name that doesn't exist.
    #[error("unknown profile reference `{0}`")]
    InvalidProfileReference(String),
}

impl ConfigError {
    /// Stable machine-readable identifier — see [`ConfigErrorCode`].
    pub const fn code(&self) -> ConfigErrorCode {
        match self {
            Self::InvalidVendorConfig { .. } => ConfigErrorCode::InvalidVendorConfig,
            Self::DuplicateTvId(_) => ConfigErrorCode::DuplicateTvId,
            Self::InvalidProfileReference(_) => ConfigErrorCode::InvalidProfileReference,
        }
    }
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
        assert_eq!(e.code(), ConfigErrorCode::InvalidVendorConfig);
    }
}
