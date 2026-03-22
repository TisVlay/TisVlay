//! D-Bus interface implementation for tisvlayd.
//!
//! Implements the multi-vendor TV control D-Bus API with:
//! - `/org/tisvlay/Manager` - Manager interface for TV management
//! - `/org/tisvlay/Tv/{id}` - Per-TV objects with feature interfaces

mod manager;
mod tv;

pub use manager::ManagerService;
pub use tv::{register_tv_interfaces, unregister_tv_interfaces, TvService};

// Re-export D-Bus path constants and helpers from tisvlay-core.
pub use tisvlay_core::dbus_paths::{
    tv_path, DAEMON_SERVICE as DBUS_NAME, MANAGER_PATH, OBJECT_MANAGER_PATH,
};

use crate::tv_manager::TvManagerError;

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

impl From<TvManagerError> for DaemonError {
    fn from(e: TvManagerError) -> Self {
        match e {
            TvManagerError::TvNotFound(id) => DaemonError::TvNotFound(id),
            TvManagerError::NotConnected(id) => {
                DaemonError::NotConnected(format!("Not connected to TV: {}", id))
            }
            TvManagerError::PoweredOff(id) => {
                DaemonError::NotConnected(format!("TV {} is powered off", id))
            }
            TvManagerError::WakeFailed { tv_id, source } => {
                DaemonError::OperationFailed(format!("Wake failed for TV {}: {}", tv_id, source))
            }
            TvManagerError::ConnectionFailed { tv_id, source } => DaemonError::ConnectionFailed(
                format!("Connection failed for TV {}: {}", tv_id, source),
            ),
            TvManagerError::CommandFailed { tv_id, source } => {
                // Inspect the underlying core error for more precise D-Bus classification
                match &source {
                    tisvlay_core::Error::NotSupported { message, .. } => {
                        DaemonError::NotSupported(message.clone())
                    }
                    tisvlay_core::Error::ConnectionClosed => {
                        DaemonError::NotConnected(format!("Connection closed for TV {}", tv_id))
                    }
                    _ => DaemonError::OperationFailed(format!(
                        "Command failed on TV {}: {}",
                        tv_id, source
                    )),
                }
            }
            TvManagerError::WakeNotSupported(id) => {
                DaemonError::NotSupported(format!("Wake not supported for TV: {}", id))
            }
            TvManagerError::PairingFailed(source) => DaemonError::PairingFailed(source.to_string()),
            TvManagerError::ConfigError(msg) => DaemonError::OperationFailed(msg),
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
