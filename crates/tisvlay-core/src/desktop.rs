//! Desktop-side identity + sync state for a single TV.
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
//! first-class notion of absence. The enum-string fields use the same
//! snake_case variants as [`SyncBacklight`](crate::SyncBacklight) and
//! [`SyncVolume`](crate::SyncVolume) for direct round-tripping.

use serde::{Deserialize, Serialize};

use crate::{SyncBacklight, SyncVolume, UserTvDesktopConfig};

/// Resolved desktop mapping and sync state for a TV.
///
/// On D-Bus this serializes as `(sssss)` —
/// `(edid, connector, sink, sync_backlight, sync_volume)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct DesktopMapping {
    /// EDID hash of the matching monitor. Empty = unknown.
    pub edid: String,
    /// DRM connector name (e.g. `HDMI-A-2`). Tiebreaker for same-model
    /// dual-TV setups. Empty = unknown / not needed.
    pub connector: String,
    /// PipeWire sink identifier. Empty = unknown.
    pub sink: String,
    /// Resolved backlight sync mode (`"none"`, `"wayland"`).
    pub sync_backlight: String,
    /// Resolved volume sync mode (`"none"`, `"mirror_hdmi"`).
    pub sync_volume: String,
}

impl DesktopMapping {
    /// Assemble from the user-config identity fields plus resolved sync modes.
    pub fn from_parts(
        identity: Option<&UserTvDesktopConfig>,
        sync_backlight: SyncBacklight,
        sync_volume: SyncVolume,
    ) -> Self {
        Self {
            edid: identity
                .and_then(|i| i.edid.as_deref())
                .unwrap_or("")
                .to_string(),
            connector: identity
                .and_then(|i| i.connector.as_deref())
                .unwrap_or("")
                .to_string(),
            sink: identity
                .and_then(|i| i.sink.as_deref())
                .unwrap_or("")
                .to_string(),
            sync_backlight: sync_backlight.to_string(),
            sync_volume: sync_volume.to_string(),
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
            sink: Some("alsa_output.hdmi".to_string()),
        };
        let m = DesktopMapping::from_parts(
            Some(&identity),
            SyncBacklight::Kernel,
            SyncVolume::MirrorHdmi,
        );
        assert_eq!(m.edid, "abc123");
        assert_eq!(m.connector, "HDMI-A-2");
        assert_eq!(m.sink, "alsa_output.hdmi");
        assert_eq!(m.sync_backlight, "kernel");
        assert_eq!(m.sync_volume, "mirror_hdmi");
    }

    #[test]
    fn from_parts_without_identity_uses_empty_strings() {
        let m = DesktopMapping::from_parts(None, SyncBacklight::None, SyncVolume::None);
        assert!(m.edid.is_empty());
        assert!(m.connector.is_empty());
        assert!(m.sink.is_empty());
        assert_eq!(m.sync_backlight, "none");
        assert_eq!(m.sync_volume, "none");
    }
}
