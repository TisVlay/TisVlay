//! KWin `kde-external-brightness-v1` Wayland forward path.
//!
//! Registers a per-TV `kde_external_brightness_device_v1`. When KWin
//! routes the brightness slider through our device it emits
//! `requested_brightness`, which we forward as `BrightnessEvent`.
//!
//! Forward-only: KWin's `set_observed_brightness` handler is one-shot
//! (commit-gated by `m_done`), so a strict Wayland backend cannot
//! drive the KDE slider from TV-side state. The orchestrator pairs
//! this with `kde_dbus::push_brightness_to_kde` when a reverse path
//! is needed.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::{RwLock, mpsc, watch};
use tracing::{debug, info, trace, warn};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use tisvlay_core::{DaemonClient, StateEvent, StateWatcher, StateWatcherConfig, TvStateChange};

use crate::SessionState;
use crate::brightness::shared::{
    BrightnessEvent, DesiredBrightness, DeviceOp, Shared, forward_brightness_events,
    read_edid_for_tv,
};
use crate::kde_external_brightness::client::{
    kde_external_brightness_device_v1::{self, KdeExternalBrightnessDeviceV1},
    kde_external_brightness_v1::{self, KdeExternalBrightnessV1},
};

const KDE_EXTERNAL_BRIGHTNESS_VERSION: u32 = 3;

// ── Wayland dispatch boilerplate ───────────────────────────────

#[derive(Debug)]
pub struct WaylandState {
    pub shared: Arc<Shared>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<KdeExternalBrightnessV1, u32> for WaylandState {
    fn event(
        _: &mut Self,
        _: &KdeExternalBrightnessV1,
        _: kde_external_brightness_v1::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<KdeExternalBrightnessDeviceV1, u32> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &KdeExternalBrightnessDeviceV1,
        event: kde_external_brightness_device_v1::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let kde_external_brightness_device_v1::Event::RequestedBrightness { value } = event;
        let map = state.shared.device_tv_map.lock().expect("poisoned");
        if let Some(tv_id) = map.get(&proxy.id().protocol_id()) {
            trace!(tv_id, value, "requested_brightness from KWin");
            let _ = state.shared.event_tx.send(BrightnessEvent::Requested {
                tv_id: tv_id.clone(),
                value,
            });
        }
    }
}

// ── Wayland thread lifecycle ───────────────────────────────────

/// Start the Wayland thread and return the negotiated protocol version
/// plus its JoinHandle. The thread drains `DeviceOp`s until
/// `Shutdown` or channel close.
pub fn start_wayland_thread(
    shared: Arc<Shared>,
    ops_rx: std::sync::mpsc::Receiver<DeviceOp>,
) -> Result<(u32, thread::JoinHandle<()>)> {
    let conn =
        Connection::connect_to_env().map_err(|e| anyhow!("Wayland connection failed: {e}"))?;
    let (globals, event_queue): (GlobalList, EventQueue<WaylandState>) =
        registry_queue_init(&conn).map_err(|e| anyhow!("Wayland registry init failed: {e}"))?;

    let manager = globals
        .contents()
        .clone_list()
        .iter()
        .find(|g| g.interface == "kde_external_brightness_v1")
        .ok_or_else(|| anyhow!("kde_external_brightness_v1 not available (KDE Plasma 6.1+)"))?
        .clone();

    let version = manager.version.min(KDE_EXTERNAL_BRIGHTNESS_VERSION);
    info!(
        "kde_external_brightness_v1 available (server: v{}, using: v{})",
        manager.version, version
    );

    let name = manager.name;
    let handle = thread::Builder::new()
        .name("wayland-brightness".into())
        .spawn(move || wayland_loop(conn, event_queue, globals, shared, ops_rx, name, version))
        .map_err(|e| anyhow!("Failed to spawn Wayland brightness thread: {e}"))?;

    Ok((version, handle))
}

fn wayland_loop(
    _conn: Connection,
    mut event_queue: EventQueue<WaylandState>,
    globals: GlobalList,
    shared: Arc<Shared>,
    ops_rx: std::sync::mpsc::Receiver<DeviceOp>,
    manager_name: u32,
    version: u32,
) {
    let qh = event_queue.handle();
    let mut state = WaylandState {
        shared: shared.clone(),
    };
    let registry = globals.registry();
    let manager: KdeExternalBrightnessV1 = registry.bind(manager_name, version, &qh, version);
    let mut devices: HashMap<String, KdeExternalBrightnessDeviceV1> = HashMap::new();

    loop {
        loop {
            match ops_rx.try_recv() {
                Ok(DeviceOp::Create {
                    tv_id,
                    edid_base64,
                    initial_brightness,
                }) => {
                    let device = manager.create_brightness_control(&qh, version);
                    device.set_internal(0);
                    device.set_edid(edid_base64);
                    device.set_max_brightness(100);
                    if version >= 2 {
                        device.set_observed_brightness(initial_brightness as u32);
                    }
                    device.commit();
                    let wl_id = device.id().protocol_id();
                    shared
                        .device_tv_map
                        .lock()
                        .expect("poisoned")
                        .insert(wl_id, tv_id.clone());
                    devices.insert(tv_id.clone(), device);
                    debug!(tv_id, wl_id, "created brightness device");
                }
                Ok(DeviceOp::Destroy { tv_id }) => {
                    if let Some(device) = devices.remove(&tv_id) {
                        shared
                            .device_tv_map
                            .lock()
                            .expect("poisoned")
                            .remove(&device.id().protocol_id());
                        device.destroy();
                    }
                }
                Ok(DeviceOp::Shutdown) => {
                    for (_, d) in devices.drain() {
                        d.destroy();
                    }
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if event_queue.flush().is_err() {
            return;
        }
        if let Some(guard) = event_queue.prepare_read() {
            let _ = guard.read();
        }
        match event_queue.dispatch_pending(&mut state) {
            Ok(0) => std::thread::sleep(Duration::from_millis(50)),
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

// ── Reconcile ──────────────────────────────────────────────────

/// Per-TV state tracked by the coordinator reconcile loop.
#[derive(Debug)]
pub struct Binding {
    pub monitor_name: Option<String>,
}

/// Reconcile Wayland brightness devices against the currently-desired
/// set for the TVs whose profile matches any of `accept_modes`.
///
/// - Sends `DeviceOp::Destroy` for bindings that no longer match.
/// - Sends `DeviceOp::Create` for newly-desired TVs.
/// - Rebuilds `shared.label_to_tv` from monitor names for the D-Bus
///   observer's routing.
pub async fn reconcile(
    bindings: &mut HashMap<String, Binding>,
    ops_tx: &std::sync::mpsc::Sender<DeviceOp>,
    shared: &Arc<Shared>,
    daemon: &DaemonClient,
    state: &Arc<RwLock<SessionState>>,
    accept_modes: &[tisvlay_core::SyncBacklight],
) {
    let tvs = match daemon.list_tvs().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "list_tvs failed during brightness reconcile");
            return;
        }
    };

    let desired: HashMap<String, DesiredBrightness> = {
        let s = state.read().await;
        tvs.iter()
            .filter_map(|tv| {
                let profile = s.profiles.get(&tv.id)?;
                if !accept_modes.contains(&profile.sync_backlight) {
                    return None;
                }
                if !tv.connected {
                    return None;
                }
                let edid = read_edid_for_tv(tv, &s)?;
                Some((
                    tv.id.clone(),
                    DesiredBrightness {
                        edid_base64: edid.base64,
                        backlight: tv.backlight,
                        monitor_name: edid.monitor_name,
                    },
                ))
            })
            .collect()
    };

    let stale: Vec<String> = bindings
        .keys()
        .filter(|id| !desired.contains_key(*id))
        .cloned()
        .collect();
    for tv_id in stale {
        let _ = ops_tx.send(DeviceOp::Destroy {
            tv_id: tv_id.clone(),
        });
        bindings.remove(&tv_id);
    }

    for (tv_id, d) in &desired {
        if bindings.contains_key(tv_id) {
            continue;
        }
        let _ = ops_tx.send(DeviceOp::Create {
            tv_id: tv_id.clone(),
            edid_base64: d.edid_base64.clone(),
            initial_brightness: d.backlight,
        });
        bindings.insert(
            tv_id.clone(),
            Binding {
                monitor_name: d.monitor_name.clone(),
            },
        );
        debug!(tv_id, "registered brightness device");
    }

    {
        let mut map = shared.label_to_tv.lock().expect("poisoned");
        map.clear();
        for (tv_id, b) in bindings.iter() {
            if let Some(name) = &b.monitor_name {
                map.insert(name.clone(), tv_id.clone());
            }
        }
    }

    debug!(
        "brightness reconcile: desired={} bound={}",
        desired.len(),
        bindings.len()
    );
}

// ── Strict Wayland-only entry point ────────────────────────────

/// Run the strict Wayland-only backend: registers devices for TVs
/// whose profile resolves to `SyncBacklight::KdeWayland`, forwards
/// `requested_brightness` to `Tv1.Backlight`. No reverse path.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: watch::Receiver<bool>,
) {
    info!("brightness: kde_wayland (strict) starting");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let shared = Shared::new(event_tx);
    let (ops_tx, ops_rx) = std::sync::mpsc::channel();

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

    let mut bindings: HashMap<String, Binding> = HashMap::new();
    let accept = [tisvlay_core::SyncBacklight::KdeWayland];

    reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state, &accept).await;

    let mut watcher = match StateWatcher::new(&daemon, StateWatcherConfig::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "StateWatcher setup failed — exiting brightness sync");
            forward_handle.abort();
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
            StreamClosed,
        }

        let action = tokio::select! {
            _ = shutdown.changed() => Action::Shutdown,
            _ = tick.tick() => Action::Tick,
            evt = watcher.next() => match evt {
                Some(StateEvent::DaemonStateChanged(
                    TvStateChange::PropertiesChanged { .. },
                ))
                | Some(StateEvent::DaemonStateChanged(_)
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
                info!("brightness: kde_wayland shutdown signalled");
                break;
            }
            Action::Tick => {
                reconcile(&mut bindings, &ops_tx, &shared, &daemon, &state, &accept).await;
            }
            Action::StreamClosed => {
                warn!("StateWatcher stream closed; exiting kde_wayland");
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
    let _ = wayland_thread.join();
}
