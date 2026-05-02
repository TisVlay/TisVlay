//! D-Bus interface implementation for tisvlayd.
//!
//! Implements the multi-vendor TV control D-Bus API with:
//! - `/org/tisvlay/Manager` - Manager interface for TV management
//! - `/org/tisvlay/Tv/{id}` - Per-TV objects with feature interfaces

mod manager;
mod setup;
mod tv;

pub use manager::ManagerService;
pub use setup::build_dbus_connection;
pub use tisvlay_core::BusKind;
pub use tv::{
    TvBacklightService, TvDesktopService, TvInputService, TvPowerService, TvScreenService,
    TvService, TvVolumeService, register_tv_interfaces, unregister_tv_interfaces,
};

// Re-export D-Bus path constants and helpers from tisvlay-core.
pub use tisvlay_core::dbus_paths::{
    DAEMON_SERVICE as DBUS_NAME, MANAGER_PATH, OBJECT_MANAGER_PATH, tv_path,
};

use crate::tv_manager::TvManagerError;

/// D-Bus error type for tisvlay.
///
/// Each variant maps to a stable D-Bus error name (e.g.
/// `org.tisvlay.Error.PowerNoMacConfigured`). Per-domain typed
/// errors from `tisvlay-core::errors` (Phase 4 of plan5) get their
/// own dedicated variants so clients can route on the name without
/// parsing the message string. Common case: GUI's inline-error
/// surface checks the D-Bus error name to pick the right localized
/// message + recovery action.
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

    // -- Power-cycle (typed; mirrors `errors::PowerErrorCode`) --
    /// TV has no wake method configured (no MAC for WoL).
    PowerNoMacConfigured(String),
    /// Wake-on-LAN packet send failed at the network layer.
    PowerWolSendFailed(String),
    /// Sent WoL but TV didn't respond within the wait window.
    PowerWakeTimeout(String),
    /// TV reachable but not accepting connections.
    PowerConnectTimeout(String),
    /// TV is already powered on.
    PowerAlreadyOn(String),
    /// TV is already powered off.
    PowerAlreadyOff(String),
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
            TvManagerError::Power { tv_id, kind } => {
                use tisvlay_core::errors::PowerError;
                let msg = format!("TV {}: {}", tv_id, kind);
                match kind {
                    PowerError::NoMacConfigured => DaemonError::PowerNoMacConfigured(msg),
                    PowerError::WolSendFailed { .. } => DaemonError::PowerWolSendFailed(msg),
                    PowerError::WakeTimeout { .. } => DaemonError::PowerWakeTimeout(msg),
                    PowerError::ConnectTimeout { .. } => DaemonError::PowerConnectTimeout(msg),
                    PowerError::AlreadyOn => DaemonError::PowerAlreadyOn(msg),
                    PowerError::AlreadyOff => DaemonError::PowerAlreadyOff(msg),
                }
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
