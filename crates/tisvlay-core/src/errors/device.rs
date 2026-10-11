//! Typed errors for device-node access.
//!
//! Cross-cutting, transport-agnostic failures a backend hits when
//! reaching a local device node (a CEC character device today; serial
//! or USB adapters in future). Kept in its own domain — rather than
//! folded into a vendor or the pairing vocabulary — because the same
//! failure (e.g. the daemon's user lacking access to `/dev/cecN`) can
//! surface from any device-node backend, and clients route on it the
//! same way regardless of which backend raised it.

use std::fmt;

/// Stable machine-readable code for a device-access failure, in the
/// `<domain>.<kebab-case-variant>` form pairing codes use too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceErrorCode {
    /// The process lacks permission to open the device node (e.g. the
    /// daemon's user isn't in the group owning `/dev/cecN`, or the udev
    /// rule granting access isn't installed).
    PermissionDenied,
}

impl DeviceErrorCode {
    /// Stable identifier for routing on clients.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "device.permission-denied",
        }
    }
}

impl fmt::Display for DeviceErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
