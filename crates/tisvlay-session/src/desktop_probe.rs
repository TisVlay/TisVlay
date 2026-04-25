//! Desktop-side identity probe.
//!
//! Enumerates DRM connectors under `/sys/class/drm/`, parses each
//! connector's EDID, and matches each against the set of TVs reported
//! by `tisvlayd` (via `Tv1.HardwareId`). Each match yields a
//! [`DesktopMapping`](tisvlay_core::DesktopMapping) the session pushes
//! to the daemon via `Manager1.SetDesktopMapping`.
//!
//! Purely read-only against the system — no DRM ioctls, no extra
//! capabilities. Reads sysfs files as the logged-in user; fails gracefully
//! when a connector has no EDID (disconnected) or the EDID is malformed.
//!
//! # Determinism
//!
//! No on-disk persistence — the probe re-runs at every session start
//! and `Session1.ReloadConfig`. To keep TV-to-connector assignments
//! stable across runs (especially for same-model dual-TV setups where
//! `HardwareId` alone can't disambiguate), the matcher sorts probed
//! displays by connector name lexicographically and assigns each
//! display to at most one TV via a greedy claim. Cross-restart
//! stability follows from sort + iteration determinism rather than
//! cached state.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use tisvlay_core::HardwareId;
use tracing::{debug, trace, warn};

/// A DRM display the probe found connected to the host.
#[derive(Debug, Clone)]
pub struct ProbedDisplay {
    /// DRM connector name (e.g. `HDMI-A-2`, derived from the sysfs
    /// directory name `card0-HDMI-A-2` → `HDMI-A-2`).
    pub connector: String,
    /// Parsed hardware identity for TV-side cross-referencing.
    pub hardware_id: HardwareId,
}

impl ProbedDisplay {
    /// Collect all connected displays with a readable EDID.
    ///
    /// Walks `/sys/class/drm/` synchronously — cheap, no I/O beyond a
    /// few small file reads per connector. Returns an empty vector if
    /// the sysfs path doesn't exist (e.g. DRM not available, running
    /// in a sandbox without sysfs).
    pub fn enumerate() -> Vec<Self> {
        Self::enumerate_at(Path::new("/sys/class/drm"))
    }

    /// Testable variant that accepts the sysfs root.
    pub fn enumerate_at(root: &Path) -> Vec<Self> {
        let Ok(entries) = fs::read_dir(root) else {
            debug!(root = %root.display(), "DRM sysfs not readable");
            return Vec::new();
        };

        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Connector directories look like `card0-HDMI-A-1`. The
            // bare `card*` and `renderD*` entries are the render nodes
            // and don't have connectors.
            let Some(connector) = dir_name.split_once('-').map(|(_, rest)| rest) else {
                continue;
            };

            match Self::parse_connector(&path, connector) {
                Ok(Some(display)) => out.push(display),
                Ok(None) => trace!(connector, "connector skipped (disconnected or no EDID)"),
                Err(e) => debug!(connector, error = %e, "failed to probe connector"),
            }
        }
        out
    }

    fn parse_connector(dir: &Path, connector: &str) -> std::io::Result<Option<Self>> {
        let status = fs::read_to_string(dir.join("status"))?;
        if status.trim() != "connected" {
            return Ok(None);
        }
        let edid = fs::read(dir.join("edid"))?;
        if edid.is_empty() {
            return Ok(None);
        }
        match parse_hardware_id(&edid) {
            Ok(hardware_id) => Ok(Some(Self {
                connector: connector.to_string(),
                hardware_id,
            })),
            Err(e) => {
                debug!(connector, error = %e, "EDID parse failed, skipping");
                Ok(None)
            }
        }
    }
}

/// Match a probe result set against TVs keyed by `HardwareId`.
///
/// Returns a map of `tv_id -> &ProbedDisplay` for each TV the probe
/// could pair with a display. Each display is claimed by at most one
/// TV — same-model dual-TV setups get distinct connectors as long as
/// there are enough probed displays to go around.
///
/// Determinism: probed displays are sorted by connector name
/// lexicographically before matching, and each TV in the input order
/// claims the highest-scoring unclaimed display. Same inputs (TV
/// list, display set) always produce the same assignment, regardless
/// of the order `read_dir` returned the displays in.
pub fn match_displays_to_tvs<'a>(
    displays: &'a [ProbedDisplay],
    tvs: &[(String, HardwareId)],
) -> std::collections::HashMap<String, &'a ProbedDisplay> {
    let mut sorted: Vec<&'a ProbedDisplay> = displays.iter().collect();
    sorted.sort_by(|a, b| a.connector.cmp(&b.connector));

    let mut claimed: HashSet<String> = HashSet::new();
    let mut out = std::collections::HashMap::new();

    for (tv_id, tv_hw) in tvs {
        let mut best: Option<&'a ProbedDisplay> = None;
        let mut best_score: u32 = 0;
        let mut tied: usize = 0;

        for d in &sorted {
            if claimed.contains(&d.connector) || !identity_matches(tv_hw, &d.hardware_id) {
                continue;
            }
            let score = match_score(tv_hw, &d.hardware_id);
            match best {
                None => {
                    best = Some(*d);
                    best_score = score;
                    tied = 1;
                }
                Some(_) if score > best_score => {
                    best = Some(*d);
                    best_score = score;
                    tied = 1;
                }
                Some(_) if score == best_score => {
                    tied += 1;
                }
                _ => {}
            }
        }

        if let Some(d) = best {
            if tied > 1 {
                warn!(
                    tv_id = %tv_id,
                    connector = %d.connector,
                    tied,
                    "Multiple unclaimed displays match by manufacturer; picked deterministically by connector-name sort"
                );
            }
            claimed.insert(d.connector.clone());
            out.insert(tv_id.clone(), d);
        }
    }

    out
}

/// Does `edid` plausibly identify the same physical device as `tv`?
///
/// Manufacturer (3-letter PNPID) is the anchor — both sides reliably
/// agree on it. Model and serial often disagree in practice: LG's
/// EDID monitor-name descriptor is a generic string like `"LG TV
/// SSCR2"` while SSAP `getSystemInfo` returns the product-level
/// `"OLED42C3PUA"`; the EDID `serial_number` is frequently a
/// boilerplate value (`0x01010101`) on LG sets rather than the unit
/// serial reported by SSAP. So we accept manufacturer-only matches
/// and rely on [`match_score`] + the connector tiebreaker to
/// disambiguate multi-TV setups.
pub(crate) fn identity_matches(tv: &HardwareId, edid: &HardwareId) -> bool {
    if tv.is_empty() || edid.is_empty() {
        return false;
    }
    match (&tv.manufacturer, &edid.manufacturer) {
        (Some(a), Some(b)) => a == b,
        // If either side doesn't know its manufacturer, we can't make
        // an educated match — skip it.
        _ => false,
    }
}

/// Relative confidence that `edid` is the exact same unit as `tv`.
///
/// Used to rank multiple manufacturer-matching candidates when a
/// single TV could plausibly map to several displays. Model and
/// serial agreement each add a point; disagreements cost nothing
/// (they're expected in practice — see [`identity_matches`]).
pub(crate) fn match_score(tv: &HardwareId, edid: &HardwareId) -> u32 {
    let mut score = 0;
    if let (Some(a), Some(b)) = (&tv.model, &edid.model) {
        if a == b {
            score += 1;
        }
    }
    if let (Some(a), Some(b)) = (&tv.serial, &edid.serial) {
        if a == b {
            score += 2;
        }
    }
    score
}

fn parse_hardware_id(edid: &[u8]) -> Result<HardwareId, edidkit::EdidError> {
    let parsed = edidkit::Edid::parse(edid)?;
    let product = parsed.product();
    let manufacturer =
        (!product.manufacturer_id.is_empty()).then(|| product.manufacturer_id.clone());
    let model = product
        .monitor_name
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| (product.product_code != 0).then(|| format!("{:04X}", product.product_code)));
    let serial = product
        .monitor_serial
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| (product.serial_number != 0).then(|| product.serial_number.to_string()));
    Ok(HardwareId {
        manufacturer,
        model,
        serial,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(manu: &str, model: &str, serial: Option<&str>) -> HardwareId {
        HardwareId::new(Some(manu), Some(model), serial)
    }

    #[test]
    fn identity_match_exact() {
        let a = hw("GSM", "OLED65G4SUB", Some("abc"));
        let b = hw("GSM", "OLED65G4SUB", Some("abc"));
        assert!(identity_matches(&a, &b));
    }

    #[test]
    fn identity_match_serial_missing_one_side() {
        let a = hw("GSM", "OLED65G4SUB", Some("abc"));
        let b = HardwareId::new(Some("GSM"), Some("OLED65G4SUB"), None::<&str>);
        assert!(identity_matches(&a, &b));
    }

    #[test]
    fn identity_mismatch_manufacturer_differs() {
        let a = hw("GSM", "X", Some("abc"));
        let b = hw("SAM", "X", Some("abc"));
        assert!(!identity_matches(&a, &b));
    }

    #[test]
    fn match_score_rewards_model_and_serial_agreement() {
        let tv = hw("GSM", "OLED65G4SUB", Some("abc"));
        let edid_exact = hw("GSM", "OLED65G4SUB", Some("abc"));
        let edid_model_only = hw("GSM", "OLED65G4SUB", Some("xyz"));
        let edid_generic = hw("GSM", "LG TV SSCR2", Some("0"));

        // All three match by manufacturer (the real-world floor).
        assert!(identity_matches(&tv, &edid_exact));
        assert!(identity_matches(&tv, &edid_model_only));
        assert!(identity_matches(&tv, &edid_generic));

        // Score ranks the exact match highest.
        assert!(match_score(&tv, &edid_exact) > match_score(&tv, &edid_model_only));
        assert!(match_score(&tv, &edid_model_only) > match_score(&tv, &edid_generic));
    }

    #[test]
    fn empty_ids_never_match() {
        let a = HardwareId::default();
        let b = hw("GSM", "OLED65G4SUB", None::<&str>);
        assert!(!identity_matches(&a, &b));
        assert!(!identity_matches(&b, &a));
    }

    /// Same-model dual-TV: each TV claims a distinct display, and the
    /// assignment is deterministic from connector-name sort + TV input
    /// order alone (no persistence needed).
    #[test]
    fn match_dual_tvs_picks_distinct_displays_deterministically() {
        let displays = vec![
            ProbedDisplay {
                connector: "HDMI-A-1".into(),
                hardware_id: hw("GSM", "M", None::<&str>),
            },
            ProbedDisplay {
                connector: "HDMI-A-2".into(),
                hardware_id: hw("GSM", "M", None::<&str>),
            },
        ];
        let tvs = vec![
            ("living".to_string(), hw("GSM", "M", None::<&str>)),
            ("office".to_string(), hw("GSM", "M", None::<&str>)),
        ];
        let out = match_displays_to_tvs(&displays, &tvs);

        assert_eq!(out.len(), 2);
        // Sorted-by-connector-name + greedy claim: living gets HDMI-A-1,
        // office gets HDMI-A-2.
        assert_eq!(out["living"].connector, "HDMI-A-1");
        assert_eq!(out["office"].connector, "HDMI-A-2");
    }

    /// Cross-restart stability: feeding the same probed displays in
    /// different orders (mimics sysfs `read_dir` reordering on a
    /// kernel update or hot-plug event) produces the same assignment.
    /// This is the property that lets us drop on-disk persistence —
    /// determinism comes from the matcher itself, not cached state.
    #[test]
    fn match_is_stable_across_read_dir_reorderings() {
        let display_a = ProbedDisplay {
            connector: "HDMI-A-1".into(),
            hardware_id: hw("GSM", "M", None::<&str>),
        };
        let display_b = ProbedDisplay {
            connector: "HDMI-A-2".into(),
            hardware_id: hw("GSM", "M", None::<&str>),
        };
        let tvs = vec![
            ("living".to_string(), hw("GSM", "M", None::<&str>)),
            ("office".to_string(), hw("GSM", "M", None::<&str>)),
        ];

        let order_1 = [display_a.clone(), display_b.clone()];
        let order_2 = [display_b, display_a];
        let out_1 = match_displays_to_tvs(&order_1, &tvs);
        let out_2 = match_displays_to_tvs(&order_2, &tvs);

        assert_eq!(out_1["living"].connector, out_2["living"].connector);
        assert_eq!(out_1["office"].connector, out_2["office"].connector);
    }

    /// Higher score (model + serial agreement) wins over a generic
    /// manufacturer-only match, regardless of connector-name order.
    #[test]
    fn higher_score_match_wins_over_lower_score() {
        let displays = vec![
            ProbedDisplay {
                connector: "HDMI-A-1".into(),
                hardware_id: hw("GSM", "LG TV SSCR2", None::<&str>),
            },
            ProbedDisplay {
                connector: "HDMI-A-2".into(),
                hardware_id: hw("GSM", "OLED65G4SUB", Some("abc")),
            },
        ];
        let tvs = vec![("office".to_string(), hw("GSM", "OLED65G4SUB", Some("abc")))];
        let out = match_displays_to_tvs(&displays, &tvs);
        // Even though HDMI-A-1 sorts first, the exact-score match on
        // HDMI-A-2 wins.
        assert_eq!(out["office"].connector, "HDMI-A-2");
    }

    /// More TVs than displays: TVs without a free claim get no entry
    /// in the output.
    #[test]
    fn fewer_displays_than_tvs_leaves_extras_unmatched() {
        let displays = vec![ProbedDisplay {
            connector: "HDMI-A-1".into(),
            hardware_id: hw("GSM", "M", None::<&str>),
        }];
        let tvs = vec![
            ("living".to_string(), hw("GSM", "M", None::<&str>)),
            ("office".to_string(), hw("GSM", "M", None::<&str>)),
        ];
        let out = match_displays_to_tvs(&displays, &tvs);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("living"));
        assert!(!out.contains_key("office"));
    }

    #[test]
    fn parse_edid_extracts_manufacturer_and_product_code() {
        // Minimal EDID header. Bytes 8-9 encode the manufacturer PNPID,
        // bytes 10-11 encode the product code. "GSM" = 0x1E6D, little
        // endian per the EDID spec. Product code 0x1234 is stored as
        // [0x34, 0x12] (little endian).
        let mut edid = vec![0u8; 128];
        // EDID magic header
        edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // PNPID "GSM":
        //   G=7  -> 0b00111
        //   S=19 -> 0b10011
        //   M=13 -> 0b01101
        // Packed big-endian as 16 bits: 0x1E6D
        edid[8] = 0x1E;
        edid[9] = 0x6D;
        edid[10] = 0x34;
        edid[11] = 0x12;
        // Serial bytes 12-15: 0x01020304
        edid[12..16].copy_from_slice(&[0x04, 0x03, 0x02, 0x01]);

        // Checksum (byte 127) = -sum(bytes[0..127]) mod 256
        let sum: u32 = edid[0..127].iter().map(|&b| b as u32).sum();
        edid[127] = (256u32 - (sum % 256)) as u8;

        let hw = parse_hardware_id(&edid).expect("parse should succeed");
        assert_eq!(hw.manufacturer.as_deref(), Some("GSM"));
        assert_eq!(hw.model.as_deref(), Some("1234"));
        // serial_number reads little-endian from bytes 12-15
        assert!(hw.serial.is_some());
    }
}
