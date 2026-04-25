//! Universal KDE brightness coordinator.
//!
//! Runs the Wayland device manager and the PowerDevil D-Bus observer
//! concurrently and lets each opt in on a per-TV basis via
//! [`BrightnessMechanism::accepts`]. A user can mix `Kde`, `KdeWayland`
//! and `KdeDbus` modes across different TVs and each TV gets exactly
//! the mechanisms it asked for. Switching a TV's mode at runtime
//! (`SetTvUserProfile` → `Tv1.Desktop.SyncBacklight` flip) is picked
//! up on the next tick of the reconcile loop without requiring a
//! session restart.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, mpsc, watch};
use tracing::{info, warn};

use tisvlay_core::{
    DaemonClient, StateEvent, StateWatcher, StateWatcherConfig, SyncBacklight, TvStateChange,
};

use crate::SessionState;
use crate::brightness::kde_dbus::{
    observe_kde_brightness_dbus, push_brightness_to_kde, reconcile_label_map,
};
use crate::brightness::kde_wayland::{Binding, reconcile, start_wayland_thread};
use crate::brightness::shared::{BrightnessMechanism, DeviceOp, Shared, forward_brightness_events};

/// Run the universal KDE coordinator. Drives every TV through the
/// mechanisms its current `sync_backlight` mode opts into. Stays
/// alive across mid-session profile flips.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: watch::Receiver<bool>,
) {
    info!("brightness: kde coordinator starting");

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

    let mut bindings: HashMap<String, Binding> = HashMap::new();
    reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state).await;
    reconcile_label_map(&shared, &daemon, &state).await;

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
                info!("brightness: kde coordinator shutdown signalled");
                break;
            }
            Action::Tick => {
                reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state).await;
                reconcile_label_map(&shared, &daemon, &state).await;
            }
            Action::Props(tv_id) => {
                push_reverse_if_accepted(&tv_id, &shared, &session_conn, &daemon, &state).await;
                reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state).await;
                reconcile_label_map(&shared, &daemon, &state).await;
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

/// Reverse path: when a TV's `Tv1.Backlight` changes, push the new
/// value back to PowerDevil if the TV's resolved mode opts into the
/// D-Bus mechanism. Suppresses our own forward-path echoes by
/// consulting `shared.last_forward_sent`.
async fn push_reverse_if_accepted(
    tv_id: &str,
    shared: &Arc<Shared>,
    session_conn: &Arc<zbus::Connection>,
    daemon: &DaemonClient,
    state: &Arc<RwLock<SessionState>>,
) {
    let Ok(tvs) = daemon.list_tvs().await else {
        return;
    };
    let Some(tv) = tvs.iter().find(|t| t.id == tv_id) else {
        return;
    };
    let mode: Option<SyncBacklight> = {
        let s = state.read().await;
        s.profiles.get(tv_id).map(|p| p.sync_backlight)
    };
    let Some(mode) = mode else { return };
    if !BrightnessMechanism::Dbus.accepts(mode) {
        return;
    }

    let backlight = tv.backlight;
    let is_echo = {
        let mut sent = shared.last_forward_sent.lock().expect("poisoned");
        if sent.get(tv_id) == Some(&backlight) {
            sent.remove(tv_id);
            true
        } else {
            false
        }
    };
    if !is_echo {
        push_brightness_to_kde(shared, session_conn, tv_id, backlight).await;
    }
}
