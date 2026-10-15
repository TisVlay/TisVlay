//! Desktop-side identity for a single TV.
//!
//! The daemon runs a DRM-EDID probe in-process whenever a managed TV's
//! `HardwareId` becomes available, matches each TV to a physical
//! connector, and caches the result in memory only — there is no
//! on-disk persistence and no cross-process push.
//!
//! The mapping carries the matching outcome only — the connector —
//! not the underlying EDID detail. Tooling that needs richer EDID
//! data (capability tables, manufacture date, vendor blocks) calls
//! the probe library directly with the connector name from this
//! mapping rather than receiving structured detail over the wire. See
//! `docs/ideas/probe-library-extraction.md` for the rationale.
//!
//! Empty string represents "unknown / not yet probed".

/// Resolved desktop mapping for a TV.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopMapping {
    /// DRM connector name (e.g. `HDMI-A-2`). Empty = unknown / not
    /// yet probed.
    pub connector: String,
}
