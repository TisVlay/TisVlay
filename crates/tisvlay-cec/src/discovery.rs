//! `CecDiscovery` — enumerate the host's CEC character devices.
//!
//! CEC has no network discovery; "discovery" here is listing the
//! `/dev/cecN` nodes the kernel exposes, so the pairing wizard can offer a
//! device picker instead of asking the user to type a path. Each node
//! becomes a `DiscoveredTv` whose address is the device path; the wizard
//! pre-fills the `device` field from `vendor_data["device"]` via the
//! schema's `autofill = "discovery.device"` directive.
//!
//! Each node is then *probed*, best-effort: the scanner opens it and queries
//! the TV's OSD name + vendor over the bus, so the wizard's list shows the
//! real set (`OLED42C3PU` / LG) and its name field prefills. A node that
//! doesn't answer (or can't be opened — e.g. already held by an active
//! connection) falls back to the bare `HDMI CEC (/dev/cecN)` entry. The
//! identity queries are bounded (see [`crate::link`]) so a silent TV can't
//! stall the scan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use linux_cec::PhysicalAddress;
use tisvlay_core::{DiscoveredTv, Error, Result, TvDiscovery};
use tracing::debug;

use crate::link::{CecLinkOpener, LinuxCecLinkOpener};

/// The physical address the probe claims when opening a node — the same
/// `1.0.0.0` (HDMI 1) default the config uses; it only matters for the
/// adapter to join the bus, and identity queries don't depend on it.
fn probe_physical_address() -> PhysicalAddress {
    PhysicalAddress::from(0x1000)
}

/// Enumerates CEC character devices under a directory (`/dev` in
/// production; a fixture dir in tests) and probes each for its identity.
#[derive(Debug)]
pub struct CecDiscovery {
    dev_dir: PathBuf,
    opener: Arc<dyn CecLinkOpener>,
}

impl CecDiscovery {
    /// Discover devices under `/dev`.
    pub fn new() -> Self {
        Self {
            dev_dir: PathBuf::from("/dev"),
            opener: Arc::new(LinuxCecLinkOpener),
        }
    }

    /// Discover devices under a custom directory with an injected opener
    /// (tests point the dir at fixture `cecN` files and the opener at a fake).
    #[cfg(test)]
    fn with_dir_and_opener(dir: impl Into<PathBuf>, opener: Arc<dyn CecLinkOpener>) -> Self {
        Self {
            dev_dir: dir.into(),
            opener,
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
        // Enumerate (fast, bounded) then probe each node for its identity.
        let mut tvs = enumerate(&self.dev_dir);
        for tv in &mut tvs {
            // Best-effort: an open failure (device busy / not a real CEC node)
            // or a TV that answers nothing leaves the bare entry intact.
            let link = match self
                .opener
                .open(&tv.address, probe_physical_address())
                .await
            {
                Ok(link) => link,
                Err(e) => {
                    // Every probe failure gets a diagnostic line — silence
                    // here is what made a permission problem read as an
                    // off/absent TV. A permission failure additionally tags
                    // the row so the wizard can badge it: the node was
                    // enumerable but the daemon's user can't open it.
                    debug!(device = %tv.address, error = %e, "CEC probe open failed");
                    if let Error::PermissionDenied { .. } = e {
                        tv.vendor_data
                            .insert("access".to_string(), "permission_denied".to_string());
                    }
                    continue;
                }
            };
            let identity = crate::identity::query(link.as_ref()).await;
            if let Some(hw) = identity.hardware_id {
                // Prefer the TV's own name (its OSD name = the model) for the
                // list + the wizard's name field; keep the path entry too.
                if let Some(model) = &hw.model {
                    tv.name = model.clone();
                }
                tv.hardware_id = Some(hw);
            }
            // Carry the resolved canonical vendor so the wizard tile brands by
            // it (`lg`) instead of the backend id (`cec`). `vendor_data` is the
            // free-form channel that already crosses D-Bus to the wizard;
            // adding a `DiscoveredTv` field would change its wire signature.
            if let Some(brand) = identity.brand {
                tv.vendor_data.insert("brand".to_string(), brand);
            }
        }
        Ok(tvs)
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
                // `source` tags how the TV was found so the wizard can show
                // a discovery-source chip; `device` carries the node path
                // for the config autofill.
                vendor_data: HashMap::from([
                    ("device".to_string(), device),
                    ("source".to_string(), "cec".to_string()),
                ]),
            }
        })
        .collect();
    // Stable, *numeric* order (cec2 before cec10) so the wizard list is
    // both stable across scans and in the order a human expects.
    tvs.sort_by_key(|tv| cec_index(&tv.address));
    tvs
}

/// The `N` in a `/dev/cecN` path, for numeric ordering. Unparseable paths
/// sort last (they shouldn't occur — `is_cec_device` gates enumeration).
fn cec_index(address: &str) -> u32 {
    address
        .rsplit("cec")
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::FakeCecLinkOpener;
    use linux_cec::operand::PowerStatus;

    /// An opener that succeeds but reports no identity — the probe then
    /// leaves each node as its bare `/dev/cecN` entry.
    fn no_identity_opener() -> Arc<dyn CecLinkOpener> {
        Arc::new(FakeCecLinkOpener::new(PowerStatus::Standby))
    }

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
        // `cec2` + `cec10` catch lexicographic-vs-numeric ordering:
        // lexicographic would put `cec10` before `cec2`.
        for name in ["cec0", "cec1", "cec2", "cec10", "video0", "cec", "null"] {
            std::fs::File::create(tmp.path().join(name)).expect("create fixture");
        }
        let discovery = CecDiscovery::with_dir_and_opener(tmp.path(), no_identity_opener());
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
        assert_eq!(addrs, ["cec0", "cec1", "cec2", "cec10"]);

        // Each carries the device path for the wizard autofill.
        let first = &tvs[0];
        assert_eq!(first.vendor, crate::VENDOR_ID);
        assert_eq!(
            first.vendor_data.get("device").map(String::as_str),
            Some(first.address.as_str())
        );
    }

    #[tokio::test]
    async fn discover_probes_identity_into_name_and_hardware_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::File::create(tmp.path().join("cec0")).expect("create fixture");
        // The opener reports an LG TV; the probe folds it into the entry.
        let opener = Arc::new(FakeCecLinkOpener::with_identity(
            "OLED42C3PU",
            Some(linux_cec::VendorId([0x00, 0xE0, 0x91])),
        ));
        let tvs = CecDiscovery::with_dir_and_opener(tmp.path(), opener)
            .discover(Duration::ZERO)
            .await
            .expect("discover");
        assert_eq!(tvs.len(), 1);
        let tv = &tvs[0];
        assert_eq!(tv.name, "OLED42C3PU", "the TV's own name replaces the path");
        let hw = tv.hardware_id.as_ref().expect("probed hardware id");
        assert_eq!(hw.manufacturer.as_deref(), Some("LG"));
        assert_eq!(hw.model.as_deref(), Some("OLED42C3PU"));
        // The canonical brand rides `vendor_data` so the wizard tile can brand
        // by it (LG) rather than the backend id (CE).
        assert_eq!(tv.vendor_data.get("brand").map(String::as_str), Some("lg"));
        // The device path is still there for the config autofill.
        assert_eq!(
            tv.vendor_data.get("device").map(String::as_str),
            Some(tv.address.as_str())
        );
    }

    #[tokio::test]
    async fn discover_flags_permission_denied_node() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::File::create(tmp.path().join("cec0")).expect("create fixture");
        // The node enumerates, but the daemon's user can't open it.
        let opener = Arc::new(FakeCecLinkOpener::permission_denied());
        let tvs = CecDiscovery::with_dir_and_opener(tmp.path(), opener)
            .discover(Duration::ZERO)
            .await
            .expect("discover");
        assert_eq!(tvs.len(), 1);
        let tv = &tvs[0];
        // Tagged so the wizard can badge it; identity stays unprobed.
        assert_eq!(
            tv.vendor_data.get("access").map(String::as_str),
            Some("permission_denied"),
        );
        assert!(tv.hardware_id.is_none());
    }

    #[tokio::test]
    async fn discover_empty_when_no_cec_devices() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::File::create(tmp.path().join("video0")).expect("create");
        let discovery = CecDiscovery::with_dir_and_opener(tmp.path(), no_identity_opener());
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
        let discovery =
            CecDiscovery::with_dir_and_opener("/nonexistent/cec/dir", no_identity_opener());
        assert!(
            discovery
                .discover(Duration::ZERO)
                .await
                .expect("discover")
                .is_empty()
        );
    }
}
