//! Full KDE brightness orchestrator — composes `kde_wayland` (forward)
//! and `kde_dbus` (forward HDR fallback + reverse) so the KDE slider
//! stays in sync with `Tv1.Backlight` under both SDR and HDR.
//!
//! Accepts TVs whose profile resolves to any of `Kde`, `KdeWayland`,
//! or `KdeDbus` so a mixed-mode configuration still gets served.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, mpsc, watch};
use tracing::{info, warn};

use tisvlay_core::{DaemonClient, StateEvent, StateWatcher, StateWatcherConfig, TvStateChange};

use crate::SessionState;
use crate::brightness::kde_dbus::{
    observe_kde_brightness_dbus, push_brightness_to_kde, reconcile_label_map,
};
use crate::brightness::kde_wayland::{Binding, reconcile, start_wayland_thread};
use crate::brightness::shared::{DeviceOp, Shared, forward_brightness_events};

/// Run the full Kde orchestrator: Wayland forward, D-Bus
/// observer forward (HDR fallback), D-Bus reverse. Serves any TV
/// whose profile is a KDE flavor (`Kde`, `KdeWayland`, `KdeDbus`).
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: watch::Receiver<bool>,
) {
    info!("brightness: kde (full) starting");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let shared = Shared::new(event_tx);
    let (ops_tx, ops_rx) = std::sync::mpsc::channel();

    let session_conn = match zbus::Connection::session().await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            warn!("Session D-Bus unavailable, brightness sync disabled: {e}");
            return;
        }
    };

    let (_version, wayland_thread) = match start_wayland_thread(shared.clone(), ops_rx) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("Wayland brightness sync unavailable: {e}");
            return;
        }
    };

    let forward_handle = tokio::spawn(forward_brightness_events(
        event_rx,
        daemon.clone(),
        shared.clone(),
    ));

    let dbus_handle = tokio::spawn(observe_kde_brightness_dbus(
        shared.clone(),
        session_conn.clone(),
    ));

    // Accept any KDE flavor — an explicit KdeWayland- or KdeDbus-only
    // user still gets the correct single subsystem doing the work
    // because the strict variants have their own dispatcher entry
    // point; this path exists for `Kde` and mixed configurations.
    let accept = [
        tisvlay_core::SyncBacklight::Kde,
        tisvlay_core::SyncBacklight::KdeWayland,
        tisvlay_core::SyncBacklight::KdeDbus,
    ];

    let mut bindings: HashMap<String, Binding> = HashMap::new();
    reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state, &accept).await;
    reconcile_label_map(&shared, &daemon, &state, &accept).await;

    let mut watcher = match StateWatcher::new(&daemon, StateWatcherConfig::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "StateWatcher setup failed — exiting brightness sync");
            forward_handle.abort();
            dbus_handle.abort();
            let _ = ops_tx.send(DeviceOp::Shutdown);
            return;
        }
    };

    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        enum Action {
            Shutdown,
            Tick,
            Props(String),
            StreamClosed,
        }

        let action = tokio::select! {
            _ = shutdown.changed() => Action::Shutdown,
            _ = tick.tick() => Action::Tick,
            evt = watcher.next() => match evt {
                Some(StateEvent::DaemonStateChanged(
                    TvStateChange::PropertiesChanged { tv_id },
                )) => Action::Props(tv_id),
                Some(StateEvent::DaemonStateChanged(_)
                    | StateEvent::FallbackPoll
                    | StateEvent::DaemonAppeared
                    | StateEvent::DaemonDisappeared
                    | StateEvent::SessionProfileChanged) => Action::Tick,
                Some(StateEvent::SessionAppeared | StateEvent::SessionDisappeared) => continue,
                None => Action::StreamClosed,
            },
        };

        match action {
            Action::Shutdown => {
                info!("brightness: kde shutdown signalled");
                break;
            }
            Action::Tick => {
                reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state, &accept).await;
                reconcile_label_map(&shared, &daemon, &state, &accept).await;
            }
            Action::Props(tv_id) => {
                // Reverse path: push to PowerDevil for any Kde-flavor TV
                // (except strict KdeWayland, which has no reverse path).
                if let Ok(tvs) = daemon.list_tvs().await {
                    if let Some(tv) = tvs.iter().find(|t| t.id == tv_id) {
                        let mode = {
                            let s = state.read().await;
                            s.profiles.get(&tv_id).map(|p| p.sync_backlight)
                        };
                        let reverse_ok = matches!(
                            mode,
                            Some(
                                tisvlay_core::SyncBacklight::Kde
                                    | tisvlay_core::SyncBacklight::KdeDbus
                            )
                        );
                        if reverse_ok {
                            let backlight = tv.backlight;
                            let is_echo = {
                                let mut sent = shared.last_forward_sent.lock().expect("poisoned");
                                if sent.get(&tv_id) == Some(&backlight) {
                                    sent.remove(&tv_id);
                                    true
                                } else {
                                    false
                                }
                            };
                            if !is_echo {
                                push_brightness_to_kde(&shared, &session_conn, &tv_id, backlight)
                                    .await;
                            }
                        }
                    }
                }
                reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state, &accept).await;
                reconcile_label_map(&shared, &daemon, &state, &accept).await;
            }
            Action::StreamClosed => {
                warn!("StateWatcher stream closed; exiting kde");
                break;
            }
        }
    }

    for tv_id in bindings.keys() {
        let _ = ops_tx.send(DeviceOp::Destroy {
            tv_id: tv_id.clone(),
        });
    }
    let _ = ops_tx.send(DeviceOp::Shutdown);
    forward_handle.abort();
    dbus_handle.abort();
    let _ = wayland_thread.join();
}
