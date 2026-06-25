//! Vendor config for the CEC backend.

use serde::{Deserialize, Serialize};

/// CEC TV configuration, stored under `[tv.cec]` in the daemon config.
///
/// CEC has no network address, credentials, or pairing secret — the only
/// thing a CEC TV needs is which kernel character device carries its bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CecVendorConfig {
    /// Path to the kernel CEC character device (e.g. `/dev/cec0`).
    ///
    /// One adapter exposes one node; a host with several CEC adapters
    /// exposes `/dev/cec0`, `/dev/cec1`, … and each TV pins the one it
    /// is reachable on.
    #[serde(default = "default_device")]
    pub device: String,
}

fn default_device() -> String {
    "/dev/cec0".to_string()
}

impl Default for CecVendorConfig {
    fn default() -> Self {
        Self {
            device: default_device(),
        }
    }
}
