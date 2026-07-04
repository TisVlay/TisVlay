//! Minimal DPMS adapter: polls the legacy sysfs `dpms` attribute of one
//! DRM connector (KWin on Wayland writes its DPMS state through to it) and
//! feeds transitions into the world as system events. The base directory is
//! injectable so tests drive it with plain files.
//!
//! `scope_events` picks the dispatch audience: the desktop probe's
//! auto-spawned watchers tag events with their connector so the world
//! routes them to the one TV matched there; the `--dpms-connector`
//! override watcher leaves events untagged (global — every automated
//! TV), which is what a setup whose EDID match failed needs.

use std::path::PathBuf;
use std::time::Duration;

use tisvlay_world::{InEvent, SystemEvent, WorldClient};
use tracing::{debug, info};

pub async fn watch_dpms(
    base: PathBuf,
    connector: String,
    scope_events: bool,
    period: Duration,
    world: WorldClient,
) {
    let dpms_path: PathBuf = base.join(&connector).join("dpms");
    let status_path: PathBuf = base.join(&connector).join("status");
    let mut last: Option<bool> = None;
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    info!(connector, dpms = %dpms_path.display(), "DPMS watcher started");
    loop {
        ticker.tick().await;
        // A powered-off TV drops its EDID link and reads `disconnected` —
        // skip rather than misread that as a DPMS transition.
        let connected = std::fs::read_to_string(&status_path)
            .map(|s| s.trim() == "connected")
            .unwrap_or(false);
        if !connected {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&dpms_path) else {
            continue;
        };
        let on = raw.trim() == "On";
        if let Some(previous) = last
            && previous != on
        {
            let event = if on {
                SystemEvent::DpmsOn
            } else {
                SystemEvent::DpmsOff
            };
            debug!(connector, ?event, "DPMS transition");
            if !world.send(InEvent::System {
                event,
                connector: scope_events.then(|| connector.clone()),
            }) {
                return;
            }
        }
        last = Some(on);
    }
}
