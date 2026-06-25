//! `CecDiscovery` — enumerate the host's CEC character devices.
//!
//! CEC has no network discovery; "discovery" here is listing the
//! `/dev/cecN` nodes the kernel exposes, so the pairing wizard can offer a
//! device picker instead of asking the user to type a path. Each node
//! becomes a `DiscoveredTv` whose address is the device path; the wizard
//! pre-fills the `device` field from `vendor_data["device"]` via the
//! schema's `autofill = "discovery.device"` directive.
//!
//! This is enumeration only — it does not open a device or query the TV's
//! OSD name over the bus (that needs the `/dev/cecN` access the daemon,
//! not a pre-pair scan, is set up for). Richer probing is a later
//! enhancement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tisvlay_core::{DiscoveredTv, Result, TvDiscovery};

/// Enumerates CEC character devices under a directory (`/dev` in
/// production; a fixture dir in tests).
#[derive(Debug)]
pub struct CecDiscovery {
    dev_dir: PathBuf,
}

impl CecDiscovery {
    /// Discover devices under `/dev`.
    pub fn new() -> Self {
        Self {
            dev_dir: PathBuf::from("/dev"),
        }
    }

    /// Discover devices under a custom directory (tests point this at a
    /// fixture dir of fake `cecN` files).
    #[cfg(test)]
    fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dev_dir: dir.into(),
        }
    }
}

impl Default for CecDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl TvDiscovery for CecDiscovery {
    async fn discover(&self, _timeout: Duration) -> Result<Vec<DiscoveredTv>> {
        // A directory listing is fast and bounded; no need to honor the
        // timeout or hop to a blocking pool.
        Ok(enumerate(&self.dev_dir))
    }
}

/// Is `name` a CEC device node — `cec` followed by one or more digits?
fn is_cec_device(name: &str) -> bool {
    name.strip_prefix("cec")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Synchronous enumeration core: read the directory and map each CEC node
/// to a [`DiscoveredTv`]. Split out so it's exercised without async.
fn enumerate(dir: &Path) -> Vec<DiscoveredTv> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // No `/dev`, unreadable, or simply no CEC devices — nothing to offer.
        return Vec::new();
    };
    let mut tvs: Vec<DiscoveredTv> = entries
        .flatten()
        .filter(|e| is_cec_device(&e.file_name().to_string_lossy()))
        .map(|e| {
            let device = e.path().to_string_lossy().into_owned();
            DiscoveredTv {
                id: device.clone(),
                name: format!("HDMI CEC ({device})"),
                address: device.clone(),
                hardware_id: None,
                vendor: crate::VENDOR_ID.to_string(),
                location: String::new(),
                vendor_data: HashMap::from([("device".to_string(), device)]),
            }
        })
        .collect();
    // Stable order so the wizard list doesn't shuffle between scans.
    tvs.sort_by(|a, b| a.address.cmp(&b.address));
    tvs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cec_device_matches_cec_n() {
        assert!(is_cec_device("cec0"));
        assert!(is_cec_device("cec1"));
        assert!(is_cec_device("cec12"));
        assert!(!is_cec_device("cec")); // no number
        assert!(!is_cec_device("cecx")); // non-digit suffix
        assert!(!is_cec_device("ceca0")); // letter then digit
        assert!(!is_cec_device("video0"));
        assert!(!is_cec_device("dri"));
    }

    #[tokio::test]
    async fn discover_lists_cec_nodes_sorted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in ["cec0", "cec1", "cec10", "video0", "cec", "null"] {
            std::fs::File::create(tmp.path().join(name)).expect("create fixture");
        }
        let discovery = CecDiscovery::with_dir(tmp.path());
        let tvs = discovery
            .discover(Duration::from_millis(0))
            .await
            .expect("discover");

        let addrs: Vec<String> = tvs
            .iter()
            .map(|t| {
                std::path::Path::new(&t.address)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(addrs, ["cec0", "cec1", "cec10"]);

        // Each carries the device path for the wizard autofill.
        let first = &tvs[0];
        assert_eq!(first.vendor, crate::VENDOR_ID);
        assert_eq!(
            first.vendor_data.get("device").map(String::as_str),
            Some(first.address.as_str())
        );
    }

    #[tokio::test]
    async fn discover_empty_when_no_cec_devices() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::File::create(tmp.path().join("video0")).expect("create");
        let discovery = CecDiscovery::with_dir(tmp.path());
        assert!(
            discovery
                .discover(Duration::ZERO)
                .await
                .expect("discover")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn discover_empty_when_dir_missing() {
        let discovery = CecDiscovery::with_dir("/nonexistent/cec/dir");
        assert!(
            discovery
                .discover(Duration::ZERO)
                .await
                .expect("discover")
                .is_empty()
        );
    }
}
