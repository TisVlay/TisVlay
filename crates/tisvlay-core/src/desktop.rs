//! Desktop-side identity for a single TV.
//!
//! Backing type for the `org.tisvlay.Tv1.Desktop` feature interface.
//! `tisvlayd` runs a DRM-EDID probe in-process whenever a managed TV's
//! `HardwareId` becomes available (on `ConnectionChanged(true)` and
//! `Manager1.ReloadConfig`), matches each TV to a physical connector,
//! and caches the result in memory only — there is no on-disk
//! persistence and no cross-process push.
//!
//! The mapping carries the matching outcome only — `(connector)` —
//! not the underlying EDID detail. Tooling that needs richer EDID
//! data (capability tables, manufacture date, vendor blocks) calls
//! the probe library directly with the connector name from this
//! mapping rather than receiving structured detail over D-Bus. See
//! [docs/ideas.md](../../../docs/ideas.md) for the rationale.
//!
//! Empty string represents "unknown / not yet probed".

use serde::{Deserialize, Serialize};

/// Resolved desktop mapping for a TV.
///
/// On D-Bus this serializes as `(s)` — `(connector)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
pub struct DesktopMapping {
    /// DRM connector name (e.g. `HDMI-A-2`). Empty = unknown / not
    /// yet probed.
    pub connector: String,
}
