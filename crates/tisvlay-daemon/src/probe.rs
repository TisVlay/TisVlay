//! Desktop identity probe (ported from the root daemon): parse each
//! connected DRM connector's EDID into a `HardwareId`, match against TV
//! identities — consumed from our own wire, the identity keys — and feed
//! matches back into the world, where they scope DPMS events per TV.
//!
//! Connector names use the full sysfs directory name (`card1-HDMI-A-1`),
//! matching what the DPMS watcher is configured with.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tisvlay_core::HardwareId;
use tisvlay_wire::keys;
use tisvlay_world::{InEvent, WorldClient};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

#[derive(Debug, Clone)]
pub(crate) struct ProbedDisplay {
    pub connector: String,
    pub hardware_id: HardwareId,
}

pub(crate) fn enumerate_at(root: &Path) -> Vec<ProbedDisplay> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // DRM connector dirs look like `card1-HDMI-A-1`.
        if !name.starts_with("card") || !name.contains('-') {
            continue;
        }
        let dir = entry.path();
        let connected = std::fs::read_to_string(dir.join("status"))
            .map(|s| s.trim() == "connected")
            .unwrap_or(false);
        if !connected {
            trace!(connector = %name, "skipped (disconnected)");
            continue;
        }
        let Ok(edid) = std::fs::read(dir.join("edid")) else {
            continue;
        };
        if edid.is_empty() {
            continue;
        }
        match parse_hardware_id(&edid) {
            Ok(hardware_id) => out.push(ProbedDisplay {
                connector: name,
                hardware_id,
            }),
            Err(e) => debug!(connector = %name, error = %e, "EDID parse failed, skipping"),
        }
    }
    out
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

/// Manufacturer is the anchor (both sides agree); model/serial often
/// diverge in practice (EDID monitor-name vs product-level model, EDID
/// boilerplate serials), so they only contribute to the score.
fn identity_matches(tv: &HardwareId, edid: &HardwareId) -> bool {
    match (&tv.manufacturer, &edid.manufacturer) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn match_score(tv: &HardwareId, edid: &HardwareId) -> u32 {
    let mut score = 0;
    if let (Some(a), Some(b)) = (&tv.model, &edid.model)
        && a == b
    {
        score += 1;
    }
    if let (Some(a), Some(b)) = (&tv.serial, &edid.serial)
        && a == b
    {
        score += 2;
    }
    score
}

/// Deterministic greedy matcher (ported): displays sort by connector name,
/// each TV claims its best-scoring unclaimed display.
pub(crate) fn match_displays_to_tvs<'a>(
    displays: &'a [ProbedDisplay],
    tvs: &[(Uuid, HardwareId)],
) -> HashMap<Uuid, &'a ProbedDisplay> {
    let mut sorted: Vec<&'a ProbedDisplay> = displays.iter().collect();
    sorted.sort_by(|a, b| a.connector.cmp(&b.connector));
    let mut claimed: HashSet<String> = HashSet::new();
    let mut out = HashMap::new();
    for (tv_id, tv_hw) in tvs {
        let mut best: Option<&'a ProbedDisplay> = None;
        let mut best_score = 0;
        for display in &sorted {
            if claimed.contains(&display.connector)
                || !identity_matches(tv_hw, &display.hardware_id)
            {
                continue;
            }
            let score = match_score(tv_hw, &display.hardware_id);
            if best.is_none() || score > best_score {
                best = Some(display);
                best_score = score;
            }
        }
        if let Some(display) = best {
            claimed.insert(display.connector.clone());
            out.insert(*tv_id, display);
        }
    }
    out
}

/// Consume identity keys from our own wire, probe sysfs, and feed matches
/// into the world. Periodic re-probe catches displays plugged in later.
pub(crate) async fn probe_task(
    session: Session,
    site: String,
    world: WorldClient,
    sysfs: PathBuf,
    period: Duration,
) {
    let subscriber = match session
        .declare_subscriber(keys::tv_all(&site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
    {
        Ok(subscriber) => subscriber,
        Err(e) => {
            warn!(%e, "desktop probe could not subscribe, exiting");
            return;
        }
    };
    let mut tvs: HashMap<Uuid, HardwareId> = HashMap::new();
    let mut assigned: HashMap<Uuid, String> = HashMap::new();
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    info!("desktop probe started");
    loop {
        tokio::select! {
            sample = subscriber.recv_async() => {
                let Ok(sample) = sample else { return };
                let Some((tv, path)) = keys::parse_tv_path(&site, sample.key_expr().as_str())
                else {
                    continue;
                };
                if path != "meta/identity" {
                    continue;
                }
                match sample.kind() {
                    SampleKind::Delete => {
                        tvs.remove(&tv);
                        assigned.remove(&tv);
                    }
                    SampleKind::Put => {
                        let value: Value =
                            serde_json::from_slice(&sample.payload().to_bytes())
                                .unwrap_or(Value::Null);
                        let hw = HardwareId {
                            manufacturer: value["make"].as_str().map(str::to_string),
                            model: value["model"].as_str().map(str::to_string),
                            serial: value["serial"].as_str().map(str::to_string),
                        };
                        if hw.manufacturer.is_some() {
                            tvs.insert(tv, hw);
                            rematch(&sysfs, &tvs, &mut assigned, &world);
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                if !tvs.is_empty() {
                    rematch(&sysfs, &tvs, &mut assigned, &world);
                }
            }
        }
    }
}

fn rematch(
    sysfs: &Path,
    tvs: &HashMap<Uuid, HardwareId>,
    assigned: &mut HashMap<Uuid, String>,
    world: &WorldClient,
) {
    let displays = enumerate_at(sysfs);
    if displays.is_empty() {
        return;
    }
    let list: Vec<(Uuid, HardwareId)> = tvs.iter().map(|(id, hw)| (*id, hw.clone())).collect();
    for (tv, display) in match_displays_to_tvs(&displays, &list) {
        if assigned.get(&tv) != Some(&display.connector) {
            assigned.insert(tv, display.connector.clone());
            world.send(InEvent::ConnectorMatched {
                tv,
                connector: display.connector.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(manufacturer: &str, model: &str, serial: Option<&str>) -> HardwareId {
        HardwareId {
            manufacturer: Some(manufacturer.into()),
            model: Some(model.into()),
            serial: serial.map(str::to_string),
        }
    }

    fn display(connector: &str, id: HardwareId) -> ProbedDisplay {
        ProbedDisplay {
            connector: connector.into(),
            hardware_id: id,
        }
    }

    #[test]
    fn manufacturer_anchors_the_match() {
        assert!(identity_matches(
            &hw("GSM", "OLED42C3PUA", None),
            &hw("GSM", "LG TV SSCR2", None)
        ));
        assert!(!identity_matches(
            &hw("GSM", "OLED42C3PUA", None),
            &hw("SAM", "OLED42C3PUA", None)
        ));
    }

    #[test]
    fn dual_tv_claims_are_deterministic_and_exclusive() {
        let tv_a = Uuid::new_v4();
        let tv_b = Uuid::new_v4();
        let displays = vec![
            display("card1-HDMI-A-2", hw("GSM", "LG TV SSCR2", None)),
            display("card1-HDMI-A-1", hw("GSM", "LG TV SSCR2", None)),
        ];
        let tvs = vec![
            (tv_a, hw("GSM", "OLED42C3PUA", None)),
            (tv_b, hw("GSM", "OLED65G4SUB", None)),
        ];
        let matches = match_displays_to_tvs(&displays, &tvs);
        assert_eq!(matches.len(), 2);
        assert_ne!(
            matches[&tv_a].connector, matches[&tv_b].connector,
            "each display claimed at most once"
        );
        // Sorted connector order → first TV in input order claims the
        // lexicographically first display.
        assert_eq!(matches[&tv_a].connector, "card1-HDMI-A-1");
    }

    #[test]
    fn serial_agreement_outranks_model() {
        let tv = Uuid::new_v4();
        let displays = vec![
            display("card1-DP-1", hw("GSM", "OLED42C3PUA", None)),
            display("card1-DP-2", hw("GSM", "other", Some("S123"))),
        ];
        let tvs = vec![(tv, hw("GSM", "OLED42C3PUA", Some("S123")))];
        let matches = match_displays_to_tvs(&displays, &tvs);
        assert_eq!(matches[&tv].connector, "card1-DP-2");
    }
}
