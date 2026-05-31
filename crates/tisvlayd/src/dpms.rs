//! Host display-connector DPMS watcher.
//!
//! Polls the legacy sysfs `dpms` attribute of each EDID-matched display
//! connector and fires per-TV [`SystemEvent::DpmsOff`] /
//! [`SystemEvent::DpmsOn`] on On <-> blanked transitions, so a
//! managed TV can mirror the power state of the host display it shares a
//! connector with. The connector match itself is owned by
//! [`crate::desktop_probe`] / [`crate::state_forwarder`]; this module
//! only observes the matched connector's power attribute.
//!
//! No event-driven primitive exists for the `dpms` attribute (udev DRM
//! uevents fire on hotplug, not DPMS property changes), so this is a
//! poll. Dispatch is scoped to the single TV whose connector changed and
//! spawned off the poll tick, so a slow `screen_on` (which may escalate
//! to a Wake-on-LAN power cycle) can't stall polling of other
//! connectors.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info};

use crate::tv_manager::{SystemEvent, TvManager};

/// Poll cadence. ~1 s wake latency is imperceptible against the TV's own
/// screen-on ramp; the sub-100 ms snap-back budget belongs to the
/// session lockscreen path, not this one.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Collapsed DPMS power state: `On`, or anything else (Off / Standby /
/// Suspend) treated as a single `Blanked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DpmsState {
    On,
    Blanked,
}

/// Parse the trimmed contents of a connector's `dpms` attribute.
fn parse_dpms(raw: &str) -> DpmsState {
    if raw.trim().eq_ignore_ascii_case("on") {
        DpmsState::On
    } else {
        DpmsState::Blanked
    }
}

/// Resolve the sysfs connector directory for a bare connector name.
///
/// The desktop mapping stores the bare connector (`HDMI-A-1`); the sysfs
/// directory carries the card prefix (`card0-HDMI-A-1`). Reuses the same
/// `split_once('-')` derivation as the probe and matches on equality, so
/// `HDMI-A-1` never resolves to `card0-HDMI-A-12`.
fn resolve_dpms_path(root: &Path, connector: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if dir_name.split_once('-').map(|(_, rest)| rest) == Some(connector) {
            return Some(path);
        }
    }
    None
}

/// Read a connector's DPMS state, or `None` when the directory is
/// unreadable or the connector is not currently `connected` — a
/// disconnected connector's `dpms` value is meaningless, and skipping it
/// keeps a stale mapping (TV powered off, link dropped) from generating
/// phantom edges.
fn read_dpms_state(dir: &Path) -> Option<DpmsState> {
    let status = std::fs::read_to_string(dir.join("status")).ok()?;
    if status.trim() != "connected" {
        return None;
    }
    let dpms = std::fs::read_to_string(dir.join("dpms")).ok()?;
    Some(parse_dpms(&dpms))
}

/// Per-connector transition detector.
#[derive(Default)]
struct EdgeDetector {
    last: HashMap<String, DpmsState>,
}

impl EdgeDetector {
    /// Record `now` for `connector` and return the transition event, if
    /// any. The first observation of a connector primes its state without
    /// firing, so the daemon coming up against an already-on (or
    /// already-blanked) panel doesn't emit a spurious edge.
    fn observe(&mut self, connector: &str, now: DpmsState) -> Option<SystemEvent> {
        match self.last.insert(connector.to_string(), now) {
            Some(prev) if prev != now => Some(match now {
                DpmsState::Blanked => SystemEvent::DpmsOff,
                DpmsState::On => SystemEvent::DpmsOn,
            }),
            _ => None,
        }
    }

    /// Forget connectors absent from `active` so a later reappearance
    /// re-primes (first observation) instead of firing a phantom edge
    /// against a stale last-state.
    fn retain(&mut self, active: &HashSet<String>) {
        self.last.retain(|connector, _| active.contains(connector));
    }
}

/// One scan pass over every managed TV with a matched connector.
///
/// Returns the set of connectors observed live this pass, so the caller
/// can prune vanished connectors from the [`EdgeDetector`].
async fn poll_once(
    manager: &Arc<TvManager>,
    root: &Path,
    edges: &mut EdgeDetector,
) -> HashSet<String> {
    let mut active = HashSet::new();

    for tv_id in manager.list_tv_ids().await {
        let Some(mapping) = manager.get_desktop_mapping(&tv_id).await else {
            continue;
        };
        if mapping.connector.is_empty() {
            continue;
        }
        let Some(dir) = resolve_dpms_path(root, &mapping.connector) else {
            continue;
        };
        let Some(state) = read_dpms_state(&dir) else {
            continue;
        };

        // Mark the connector live before any early-out below so `retain`
        // preserves its last-state across a skipped tick.
        active.insert(mapping.connector.clone());

        // Don't observe (and thus consume the edge) while a reconnect /
        // WoL is mid-flight — re-evaluate next tick once it settles so
        // the transition isn't lost, and we don't stack a second
        // `power_on`.
        if manager.is_reconnecting(&tv_id).await {
            debug!(tv_id = %tv_id, connector = %mapping.connector,
                "DPMS dispatch deferred: reconnect in flight");
            continue;
        }

        let Some(event) = edges.observe(&mapping.connector, state) else {
            continue;
        };

        info!(tv_id = %tv_id, connector = %mapping.connector, ?event, "DPMS transition");

        // Dispatch off-tick: `screen_on`/`screen_off` may block for
        // seconds (WoL escalation), and the poll must keep servicing
        // other connectors.
        let manager = manager.clone();
        let tv_id = tv_id.clone();
        tokio::spawn(async move {
            manager
                .execute_system_event_for_tv(&tv_id, event, "dpms")
                .await;
        });
    }

    active
}

/// Spawn the background DPMS watcher. No-op-degrades when `/sys/class/drm`
/// is unreadable (sandbox / no DRM): the poll simply resolves no
/// connectors and keeps ticking cheaply.
pub fn spawn_dpms_monitor(manager: Arc<TvManager>) {
    tokio::spawn(async move {
        watch_dpms(manager, Path::new("/sys/class/drm"), POLL_INTERVAL).await;
    });
}

async fn watch_dpms(manager: Arc<TvManager>, root: &Path, interval: Duration) {
    info!("DPMS connector monitor started");
    let mut edges = EdgeDetector::default();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let active = poll_once(&manager, root, &mut edges).await;
        edges.retain(&active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::BackendRegistry;
    use std::fs;
    use tisvlay_core::{DesktopMapping, SystemConfig};

    #[test]
    fn parse_dpms_collapses_non_on_to_blanked() {
        assert_eq!(parse_dpms("On"), DpmsState::On);
        assert_eq!(parse_dpms("on\n"), DpmsState::On);
        assert_eq!(parse_dpms("  On  "), DpmsState::On);
        assert_eq!(parse_dpms("Off"), DpmsState::Blanked);
        assert_eq!(parse_dpms("Standby"), DpmsState::Blanked);
        assert_eq!(parse_dpms("Suspend"), DpmsState::Blanked);
        assert_eq!(parse_dpms("garbage"), DpmsState::Blanked);
    }

    /// Build a fake sysfs connector dir `<root>/card1-<connector>/` with
    /// the given `status` and `dpms` contents.
    fn write_connector(root: &Path, connector: &str, status: &str, dpms: &str) {
        let dir = root.join(format!("card1-{connector}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("status"), status).unwrap();
        fs::write(dir.join("dpms"), dpms).unwrap();
    }

    #[test]
    fn resolve_dpms_path_matches_on_connector_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_connector(root, "HDMI-A-1", "connected", "On");
        write_connector(root, "HDMI-A-12", "connected", "On");

        // Exact bare-connector match, not a prefix of the longer name.
        let resolved = resolve_dpms_path(root, "HDMI-A-1").unwrap();
        assert!(resolved.ends_with("card1-HDMI-A-1"));
        assert!(resolve_dpms_path(root, "DP-1").is_none());
    }

    #[test]
    fn read_dpms_state_skips_disconnected_connector() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_connector(root, "HDMI-A-1", "connected", "On");
        write_connector(root, "HDMI-A-2", "disconnected", "On");

        let dir1 = resolve_dpms_path(root, "HDMI-A-1").unwrap();
        let dir2 = resolve_dpms_path(root, "HDMI-A-2").unwrap();
        assert_eq!(read_dpms_state(&dir1), Some(DpmsState::On));
        // A disconnected connector's dpms value is meaningless → None.
        assert_eq!(read_dpms_state(&dir2), None);
    }

    #[test]
    fn edge_detector_primes_then_fires_transitions() {
        let mut edges = EdgeDetector::default();
        // First observation primes without firing.
        assert_eq!(edges.observe("HDMI-A-1", DpmsState::On), None);
        // No change → no event.
        assert_eq!(edges.observe("HDMI-A-1", DpmsState::On), None);
        // On → Blanked.
        assert!(matches!(
            edges.observe("HDMI-A-1", DpmsState::Blanked),
            Some(SystemEvent::DpmsOff)
        ));
        // Blanked → On.
        assert!(matches!(
            edges.observe("HDMI-A-1", DpmsState::On),
            Some(SystemEvent::DpmsOn)
        ));
    }

    #[test]
    fn edge_detector_retain_re_primes_dropped_connectors() {
        let mut edges = EdgeDetector::default();
        assert_eq!(edges.observe("HDMI-A-1", DpmsState::On), None);
        // Connector vanishes from the active set → forgotten.
        edges.retain(&HashSet::new());
        // Reappearing primes again (first observation), no phantom edge.
        assert_eq!(edges.observe("HDMI-A-1", DpmsState::Blanked), None);
    }

    /// Construct a manager holding a single unmanaged TV (so the spawned
    /// dispatch is a no-op) whose desktop mapping points at `connector`.
    async fn manager_with_mapped_tv(connector: &str) -> Arc<TvManager> {
        let toml = r#"
[[tv]]
uuid = "33333333-3333-3333-3333-333333333333"
name = "Test"
vendor = "lg"
managed = false

[tv.lg]
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:FF"
"#;
        let config: SystemConfig = toml::from_str(toml).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = TvManager::new(
            config,
            dir.path().join("config.toml"),
            Arc::new(BackendRegistry::new()),
        );
        let manager = Arc::new(manager);
        manager
            .set_desktop_mapping(
                "33333333-3333-3333-3333-333333333333",
                DesktopMapping {
                    connector: connector.to_string(),
                },
            )
            .await;
        manager
    }

    #[tokio::test]
    async fn poll_once_detects_transition_through_the_manager() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        write_connector(&root, "HDMI-A-1", "connected", "On");
        let manager = manager_with_mapped_tv("HDMI-A-1").await;
        let mut edges = EdgeDetector::default();

        // First pass primes the connector.
        let active = poll_once(&manager, &root, &mut edges).await;
        assert!(active.contains("HDMI-A-1"));
        assert_eq!(edges.last.get("HDMI-A-1"), Some(&DpmsState::On));

        // Blank the panel → the next pass records the transition.
        write_connector(&root, "HDMI-A-1", "connected", "Off");
        let active = poll_once(&manager, &root, &mut edges).await;
        assert!(active.contains("HDMI-A-1"));
        assert_eq!(edges.last.get("HDMI-A-1"), Some(&DpmsState::Blanked));
    }

    #[tokio::test]
    async fn poll_once_skips_disconnected_connector() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        write_connector(&root, "HDMI-A-1", "disconnected", "On");
        let manager = manager_with_mapped_tv("HDMI-A-1").await;
        let mut edges = EdgeDetector::default();

        let active = poll_once(&manager, &root, &mut edges).await;
        assert!(active.is_empty());
        assert!(edges.last.is_empty());
    }
}
