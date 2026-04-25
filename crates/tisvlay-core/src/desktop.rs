//! Desktop-side identity for a single TV.
//!
//! This is the D-Bus payload for `Manager1.SetDesktopMapping` and the
//! backing type for the `org.tisvlay.Tv1.Desktop` feature interface.
//! The session service owns file storage (under `~/.config/tisvlay/`)
//! and pushes the live mapping to the daemon via `SetDesktopMapping`
//! whenever it changes; the daemon holds the result in memory and
//! republishes it on the per-TV D-Bus object so tray / GUI / CLI
//! consumers read from a single authoritative surface.
//!
//! Fields use the empty-string convention for unknowns (rather than
//! `Option<String>`) because the D-Bus type system doesn't have a
//! first-class notion of absence.

use serde::{Deserialize, Serialize};

use crate::UserTvDesktopConfig;

/// Resolved desktop mapping for a TV.
///
/// On D-Bus this serializes as `(ss)` — `(edid, connector)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct DesktopMapping {
    /// EDID hash of the matching monitor. Empty = unknown.
    pub edid: String,
    /// DRM connector name (e.g. `HDMI-A-2`). Tiebreaker for same-model
    /// dual-TV setups. Empty = unknown / not needed.
    pub connector: String,
}

impl DesktopMapping {
    /// Assemble from the user-config identity fields.
    pub fn from_parts(identity: Option<&UserTvDesktopConfig>) -> Self {
        Self {
            edid: identity
                .and_then(|i| i.edid.as_deref())
                .unwrap_or("")
                .to_string(),
            connector: identity
                .and_then(|i| i.connector.as_deref())
                .unwrap_or("")
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_parts_with_full_identity() {
        let identity = UserTvDesktopConfig {
            edid: Some("abc123".to_string()),
            connector: Some("HDMI-A-2".to_string()),
        };
        let m = DesktopMapping::from_parts(Some(&identity));
        assert_eq!(m.edid, "abc123");
        assert_eq!(m.connector, "HDMI-A-2");
    }

    #[test]
    fn from_parts_without_identity_uses_empty_strings() {
        let m = DesktopMapping::from_parts(None);
        assert!(m.edid.is_empty());
        assert!(m.connector.is_empty());
    }
}
