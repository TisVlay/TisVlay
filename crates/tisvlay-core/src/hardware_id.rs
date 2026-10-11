//! Vendor-agnostic hardware identity for a TV.
//!
//! Backends that can read their own identity — via vendor protocol, SSDP
//! headers, CEC OSD name, or similar — populate a [`HardwareId`] for
//! cross-referencing with EDID data read from the host's display stack.
//! Used by the desktop-display matcher to map a managed TV to the
//! monitor it presents as on Linux, and exposed on D-Bus for tooling
//! to key per-TV automations on a stable identity.
//!
//! Backends that can't read their identity return `None` from
//! [`TvConnection::hardware_id`](crate::TvConnection::hardware_id).
//!
//! # D-Bus representation
//!
//! On D-Bus this serializes as a `(sss)` struct of
//! `(manufacturer, model, serial)` with empty strings representing
//! unknown fields. Conversions are provided via `From` impls on the
//! tuple type so callers on the wire side don't need to reach for
//! serde.
//!
//! Manufacturer codes follow the EDID PNPID convention (three
//! uppercase letters, e.g. `GSM` for LG, `SAM` for Samsung) to make
//! cross-referencing with parsed EDID descriptors mechanical.

use serde::{Deserialize, Serialize};

/// Hardware identity for a TV, as reported by the backend.
///
/// Any combination of fields may be absent depending on what the
/// backend can read. Matching against an EDID-derived identity
/// should fall back from `(manufacturer, model, serial)` → `(manufacturer,
/// model)` when `serial` is missing (common for CEC or discovery-only
/// backends that never open a control session).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HardwareId {
    /// EDID PNPID-style three-letter manufacturer code (e.g. `GSM` for LG).
    pub manufacturer: Option<String>,
    /// Model name or product code (e.g. `OLED65G4SUB`).
    pub model: Option<String>,
    /// Unit serial number.
    pub serial: Option<String>,
}

impl HardwareId {
    /// Create a new hardware identity. All fields optional — pass `None`
    /// when the backend can't read that field.
    pub fn new(
        manufacturer: Option<impl Into<String>>,
        model: Option<impl Into<String>>,
        serial: Option<impl Into<String>>,
    ) -> Self {
        Self {
            manufacturer: manufacturer.map(Into::into),
            model: model.map(Into::into),
            serial: serial.map(Into::into),
        }
    }

    /// `true` if every field is absent.
    pub fn is_empty(&self) -> bool {
        self.manufacturer.is_none() && self.model.is_none() && self.serial.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_empty_detects_all_none() {
        assert!(HardwareId::default().is_empty());
        assert!(!HardwareId::new(Some("GSM"), None::<&str>, None::<&str>).is_empty());
    }
}
