//! D-Bus interface implementation for tisvlayd.
//!
//! Implements the multi-vendor TV control D-Bus API with:
//! - `/org/tisvlay/Manager` - Manager interface for TV management
//! - `/org/tisvlay/Tv/{id}` - Per-TV objects with feature interfaces

mod manager;
mod tv;

pub use manager::ManagerService;
pub use tv::{register_tv_interfaces, TvService};

// Re-export D-Bus path constants and helpers from tisvlay-core.
pub use tisvlay_core::dbus_paths::{tv_path, DAEMON_SERVICE as DBUS_NAME, MANAGER_PATH};

/// D-Bus error type for tisvlay.
#[derive(Debug, Clone, zbus::DBusError)]
#[zbus(prefix = "org.tisvlay.Error")]
pub enum DaemonError {
    /// TV not found.
    TvNotFound(String),
    /// TV not connected.
    NotConnected(String),
    /// Connection failed.
    ConnectionFailed(String),
    /// Operation failed.
    OperationFailed(String),
    /// Capability not supported.
    NotSupported(String),
    /// Unknown vendor.
    UnknownVendor(String),
    /// Pairing error.
    PairingFailed(String),
}

impl From<String> for DaemonError {
    fn from(s: String) -> Self {
        if s.contains("not found") {
            DaemonError::TvNotFound(s)
        } else if s.contains("Not connected") || s.contains("Connection is closed") {
            DaemonError::NotConnected(s)
        } else if s.contains("Connection failed") {
            DaemonError::ConnectionFailed(s)
        } else if s.contains("not supported") {
            DaemonError::NotSupported(s)
        } else if s.contains("Unknown vendor") {
            DaemonError::UnknownVendor(s)
        } else {
            DaemonError::OperationFailed(s)
        }
    }
}

impl From<tisvlay_core::Error> for DaemonError {
    fn from(e: tisvlay_core::Error) -> Self {
        match &e {
            tisvlay_core::Error::TvNotFound(id) => DaemonError::TvNotFound(id.clone()),
            tisvlay_core::Error::ConnectionClosed => {
                DaemonError::NotConnected("Connection closed".to_string())
            }
            tisvlay_core::Error::NotSupported { message, .. } => {
                DaemonError::NotSupported(message.clone())
            }
            tisvlay_core::Error::UnknownVendor(v) => DaemonError::UnknownVendor(v.clone()),
            tisvlay_core::Error::PairingRejected => {
                DaemonError::PairingFailed("Pairing rejected by TV".to_string())
            }
            tisvlay_core::Error::PairingTimeout => {
                DaemonError::PairingFailed("Pairing timed out".to_string())
            }
            _ => DaemonError::OperationFailed(e.to_string()),
        }
    }
}
