//! Desktop-side identity for a single TV.
//!
//! This is the D-Bus payload for `Manager1.SetDesktopMapping` and the
//! backing type for the `org.tisvlay.Tv1.Desktop` feature interface.
//! The session service runs a DRM-EDID probe at startup, matches each
//! managed TV to a physical connector, and pushes the result to the
//! daemon via `SetDesktopMapping`. The daemon caches in memory only;
//! there is no on-disk persistence — the probe re-runs at every
//! session start and `Session1.ReloadConfig`.
//!
//! Fields use the empty-string convention for unknowns (rather than
//! `Option<String>`) because the D-Bus type system doesn't have a
//! first-class notion of absence.

use serde::{Deserialize, Serialize};

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
