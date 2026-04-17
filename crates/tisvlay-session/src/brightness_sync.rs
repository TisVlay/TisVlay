//! Brightness sync — KDE's brightness slider drives `Tv1.Backlight`.
//!
//! For every managed TV whose effective user profile selects
//! `sync_backlight = "backlight"` and whose backend advertises
//! `Capabilities::BACKLIGHT`, the session service registers a
//! `kde_external_brightness_device_v1` with KWin so the desktop
//! brightness slider controls the TV's OLED pixel brightness.
//!
//! # Two forward paths
//!
//! KWin has two brightness modes depending on HDR state:
//!
//! **SDR mode** (HDR disabled): KWin routes slider changes through
//! the brightness device. Our Wayland client receives
//! `requested_brightness` events and forwards to `Tv1.Backlight`.
//!
//! **HDR mode** (HDR enabled): KWin uses the slider to control
//! tone-mapping reference luminance and does NOT call
//! `setBrightness` on the device (`drm_output.cpp:630`). However,
//! the `org.kde.ScreenBrightness.Display.Brightness` D-Bus property
//! still changes. A D-Bus observer catches these changes and
//! forwards them to `Tv1.Backlight`, scaled from KWin's 0-10000
//! range to our 0-100 range.
//!
//! Both paths feed into the same drain-loop and coalesce bursts.
//!
//! # Reverse path (TV → KDE slider)
//!
//! `StateWatcher` delivers `PropertiesChanged` from `Tv1.Backlight`.
//! We call `set_observed_brightness` on the Wayland device to update
//! KDE's displayed value (requires protocol version ≥ 2).
//!
//! # Lifecycle
//!
//! Devices are created/destroyed by the coordinator on every
//! reconcile pass (10 s tick + every `StateWatcher` event): TV
//! connected + profile says `"backlight"` + EDID available → create;
//! TV disconnected / profile flips to `"none"` / no EDID → destroy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tisvlay_core::{DaemonClient, StateEvent, StateWatcher, StateWatcherConfig, TvStateChange};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, trace, warn};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::SessionState;
use crate::kde_external_brightness::client::{
    kde_external_brightness_device_v1::{self, KdeExternalBrightnessDeviceV1},
    kde_external_brightness_v1::KdeExternalBrightnessV1,
};

const KDE_EXTERNAL_BRIGHTNESS_VERSION: u32 = 3;

/// Events from the Wayland thread into the tokio coordinator.
#[derive(Debug, Clone)]
pub enum BrightnessEvent {
    Requested { tv_id: String, value: u32 },
}

/// Shared state accessible from both the Wayland thread and tokio.
/// The Wayland thread writes `requested_brightness` events; the tokio
/// side reads them and manages device lifecycle.
struct Shared {
    event_tx: mpsc::UnboundedSender<BrightnessEvent>,
    /// Map from Wayland object id to tv_id for routing callbacks.
    device_tv_map: Mutex<HashMap<u32, String>>,
}

/// Per-TV metadata tracked on the tokio side. The actual Wayland device
/// proxy lives on the Wayland thread; we route all Wayland calls through
/// the ops channel.
struct BindingMeta {
    /// Last brightness we pushed to KDE (reverse path echo suppression).
    last_observed: Option<u8>,
    /// EDID monitor name for D-Bus label matching (e.g. "LG TV SSCR2").
    monitor_name: Option<String>,
}

struct WaylandState {
    shared: Arc<Shared>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<KdeExternalBrightnessV1, u32> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &KdeExternalBrightnessV1,
        _event: kde_external_brightness_v1::Event,
        _data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<KdeExternalBrightnessDeviceV1, u32> for WaylandState {
    fn event(
        state: &mut Self,
        proxy: &KdeExternalBrightnessDeviceV1,
        event: kde_external_brightness_device_v1::Event,
        _data: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            kde_external_brightness_device_v1::Event::RequestedBrightness { value } => {
                let map = state.shared.device_tv_map.lock().expect("lock poisoned");
                if let Some(tv_id) = map.get(&proxy.id().protocol_id()) {
                    trace!(tv_id, value, "requested_brightness from compositor");
                    let _ = state
                        .shared
                        .event_tx
                        .send(BrightnessEvent::Requested {
                            tv_id: tv_id.clone(),
                            value,
                        });
                }
            }
        }
    }
}

use crate::kde_external_brightness::client::kde_external_brightness_v1;

/// Coordinator state on the tokio side.
struct BrightnessSync {
    /// Pending device operations queued from tokio, consumed by the
    /// Wayland thread on its next dispatch cycle.
    ops_tx: std::sync::mpsc::Sender<DeviceOp>,
    /// Negotiated protocol version.
    version: u32,
    /// Tracks which TVs have active devices.
    bindings: HashMap<String, BindingMeta>,
    /// KDE display label → tv_id for the D-Bus observer. Rebuilt on
    /// every reconcile from the bindings' `monitor_name` fields.
    label_to_tv: Arc<Mutex<HashMap<String, String>>>,
    /// Last brightness value sent to each TV by the forward path.
    /// Used by `push_observed_brightness` to suppress echoes.
    last_forward_sent: Arc<Mutex<HashMap<String, u8>>>,
}

enum DeviceOp {
    Create {
        tv_id: String,
        edid_base64: String,
        initial_brightness: u8,
    },
    Destroy {
        tv_id: String,
    },
    SetObservedBrightness {
        tv_id: String,
        value: u8,
    },
    Shutdown,
}

/// Run the brightness sync coordinator until shutdown.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    info!("brightness_sync task starting");

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        event_tx,
        device_tv_map: Mutex::new(HashMap::new()),
    });

    let (ops_tx, ops_rx) = std::sync::mpsc::channel();

    // Start the Wayland event loop on a dedicated thread.
    let wayland_shared = shared.clone();
    let (version, wayland_thread) = match start_wayland_thread(wayland_shared, ops_rx) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("Wayland brightness sync unavailable: {e}");
            return;
        }
    };

    let label_to_tv = Arc::new(Mutex::new(HashMap::new()));
    let last_forward_sent = Arc::new(Mutex::new(HashMap::new()));

    let forward_handle = tokio::spawn(forward_brightness_events(
        event_rx,
        daemon.clone(),
        last_forward_sent.clone(),
    ));

    // D-Bus observer for HDR mode: subscribes to PropertiesChanged
    // on org.kde.ScreenBrightness displays. Uses label_to_tv for
    // mapping display labels to TV IDs (rebuilt on every reconcile).
    let dbus_observer_handle = tokio::spawn(observe_kde_brightness_dbus(
        shared.event_tx.clone(),
        label_to_tv.clone(),
    ));

    let mut sync = BrightnessSync {
        ops_tx,
        version,
        bindings: HashMap::new(),
        label_to_tv,
        last_forward_sent,
    };

    reconcile(&mut sync, &daemon, &state).await;

    let mut watcher = match StateWatcher::new(&daemon, StateWatcherConfig::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "StateWatcher setup failed — exiting brightness sync");
            forward_handle.abort();
            let _ = sync.ops_tx.send(DeviceOp::Shutdown);
            return;
        }
    };

    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // Collect the action from select!, then process outside.
        // D-Bus calls from inside the watcher.next() arm deadlock
        // because the zbus connection's message stream is borrowed.
        enum Action {
            Shutdown,
            Reconcile,
            PropertiesChanged(String),
            StreamClosed,
        }

        let action = tokio::select! {
            _ = shutdown.changed() => Action::Shutdown,
            _ = tick.tick() => Action::Reconcile,
            evt = watcher.next() => {
                match evt {
                    Some(StateEvent::DaemonStateChanged(TvStateChange::PropertiesChanged { tv_id })) => {
                        Action::PropertiesChanged(tv_id)
                    }
                    Some(StateEvent::DaemonStateChanged(_))
                    | Some(StateEvent::FallbackPoll)
                    | Some(StateEvent::DaemonAppeared)
                    | Some(StateEvent::DaemonDisappeared)
                    | Some(StateEvent::SessionProfileChanged) => Action::Reconcile,
                    Some(StateEvent::SessionAppeared | StateEvent::SessionDisappeared) => continue,
                    None => Action::StreamClosed,
                }
            }
        };

        match action {
            Action::Shutdown => {
                info!("Brightness sync shutdown signalled");
                break;
            }
            Action::Reconcile => {
                reconcile(&mut sync, &daemon, &state).await;
            }
            Action::PropertiesChanged(tv_id) => {
                // Spawn on a separate task to avoid deadlocking the
                // zbus connection — the StateWatcher's internal stream
                // state can block same-connection D-Bus calls even
                // after the select! arm completes.
                let d = daemon.clone();
                let ops = sync.ops_tx.clone();
                let fwd = sync.last_forward_sent.clone();
                let version = sync.version;
                if let Some(meta) = sync.bindings.get(&tv_id) {
                    let last_obs = meta.last_observed;
                    let tv = tv_id.clone();
                    tokio::spawn(async move {
                        push_observed_brightness_task(&d, &tv, version, last_obs, &ops, &fwd).await;
                    });
                }
                // Update last_observed from the spawned task's result
                // on the next reconcile (it reads fresh state anyway).
                reconcile(&mut sync, &daemon, &state).await;
            }
            Action::StreamClosed => {
                warn!("StateWatcher stream closed; exiting brightness sync");
                break;
            }
        }
    }

    // Teardown: destroy all devices, signal the Wayland thread to quit.
    for tv_id in sync.bindings.keys().cloned().collect::<Vec<_>>() {
        let _ = sync.ops_tx.send(DeviceOp::Destroy {
            tv_id: tv_id.clone(),
        });
    }
    sync.bindings.clear();
    let _ = sync.ops_tx.send(DeviceOp::Shutdown);
    forward_handle.abort();
    dbus_observer_handle.abort();
    let _ = wayland_thread.join();
}

fn start_wayland_thread(
    shared: Arc<Shared>,
    ops_rx: std::sync::mpsc::Receiver<DeviceOp>,
) -> Result<(u32, thread::JoinHandle<()>)> {
    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow!("Wayland connection failed: {e}"))?;

    let (globals, event_queue): (GlobalList, EventQueue<WaylandState>) =
        registry_queue_init(&conn)
            .map_err(|e| anyhow!("Wayland registry init failed: {e}"))?;

    let global_list = globals.contents().clone_list();
    let manager_global = global_list
        .iter()
        .find(|g| g.interface == "kde_external_brightness_v1")
        .ok_or_else(|| {
            anyhow!("kde_external_brightness_v1 not available (requires KDE Plasma 6.1+)")
        })?
        .clone();

    let version = manager_global
        .version
        .min(KDE_EXTERNAL_BRIGHTNESS_VERSION);
    info!(
        "kde_external_brightness_v1 available (server: v{}, using: v{})",
        manager_global.version, version
    );

    let handle = thread::Builder::new()
        .name("wayland-brightness".to_string())
        .spawn(move || {
            run_wayland_loop(conn, event_queue, globals, shared, ops_rx, manager_global.name, version);
        })
        .map_err(|e| anyhow!("Failed to spawn Wayland brightness thread: {e}"))?;

    Ok((version, handle))
}

fn run_wayland_loop(
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
    let manager: KdeExternalBrightnessV1 =
        registry.bind(manager_name, version, &qh, version);

    // Device proxies live here on the Wayland thread, keyed by tv_id.
    let mut devices: HashMap<String, KdeExternalBrightnessDeviceV1> = HashMap::new();

    loop {
        // Drain all pending ops from the tokio side.
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
                        .expect("lock poisoned")
                        .insert(wl_id, tv_id.clone());
                    devices.insert(tv_id.clone(), device);
                    debug!(tv_id, wl_id, "created brightness device");
                }
                Ok(DeviceOp::Destroy { tv_id }) => {
                    if let Some(device) = devices.remove(&tv_id) {
                        let wl_id = device.id().protocol_id();
                        shared
                            .device_tv_map
                            .lock()
                            .expect("lock poisoned")
                            .remove(&wl_id);
                        device.destroy();
                    }
                    debug!(tv_id, "destroyed brightness device");
                }
                Ok(DeviceOp::SetObservedBrightness { tv_id, value }) => {
                    if version >= 2 {
                        if let Some(device) = devices.get(&tv_id) {
                            device.set_observed_brightness(value as u32);
                            device.commit();
                        }
                    }
                }
                Ok(DeviceOp::Shutdown) => {
                    for (_, device) in devices.drain() {
                        device.destroy();
                    }
                    debug!("Wayland brightness thread shutting down");
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }

        // Flush outgoing requests, then non-blocking dispatch +
        // short sleep so we periodically check ops_rx for Shutdown.
        if event_queue.flush().is_err() {
            warn!("Wayland brightness flush error; exiting");
            return;
        }

        // prepare_read + read performs a non-blocking read from the
        // Wayland socket, buffering any new events.
        if let Some(guard) = event_queue.prepare_read() {
            let _ = guard.read();
        }
        match event_queue.dispatch_pending(&mut state) {
            Ok(0) => {
                // No events dispatched — sleep briefly to avoid
                // busy-looping while still checking ops_rx promptly.
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(_) => {}
            Err(_) => {
                warn!("Wayland brightness dispatch error; exiting");
                return;
            }
        }
    }
}

/// Push TV-side brightness to KDE via `set_observed_brightness`.
/// Runs as a spawned task to avoid deadlocking the shared zbus
/// connection (the StateWatcher's internal stream state blocks
/// same-connection D-Bus calls even after the select! arm completes).
async fn push_observed_brightness_task(
    daemon: &DaemonClient,
    tv_id: &str,
    version: u32,
    last_observed: Option<u8>,
    ops_tx: &std::sync::mpsc::Sender<DeviceOp>,
    last_forward_sent: &Mutex<HashMap<String, u8>>,
) {
    if version < 2 {
        return;
    }
    let backlight = match daemon.get_backlight(tv_id).await {
        Ok(v) => v,
        Err(_) => return,
    };
    // Suppress echo from our own forward-path writes.
    {
        let mut sent = last_forward_sent.lock().expect("lock poisoned");
        if sent.get(tv_id) == Some(&backlight) {
            sent.remove(tv_id);
            return;
        }
    }
    if last_observed == Some(backlight) {
        return;
    }
    let _ = ops_tx.send(DeviceOp::SetObservedBrightness {
        tv_id: tv_id.to_string(),
        value: backlight,
    });
    trace!(tv_id, backlight, "pushed observed brightness to compositor");
}

/// Forward brightness events from KDE to the TV via the daemon.
async fn forward_brightness_events(
    mut rx: mpsc::UnboundedReceiver<BrightnessEvent>,
    daemon: Arc<DaemonClient>,
    last_sent: Arc<Mutex<HashMap<String, u8>>>,
) {
    while let Some(first) = rx.recv().await {
        let mut latest: HashMap<String, u32> = HashMap::new();
        let BrightnessEvent::Requested { tv_id, value } = &first;
        latest.insert(tv_id.clone(), *value);
        while let Ok(next) = rx.try_recv() {
            let BrightnessEvent::Requested { tv_id, value } = next;
            latest.insert(tv_id, value);
        }
        for (tv_id, value) in latest {
            let level = (value as u8).min(100);
            // Record before sending so the reverse path can suppress
            // the echo when the daemon reports the same value back.
            last_sent
                .lock()
                .expect("lock poisoned")
                .insert(tv_id.clone(), level);
            if let Err(e) = daemon.set_backlight(&tv_id, level).await {
                warn!(tv_id, level, error = %e, "forward brightness set failed");
            }
        }
    }
}

/// D-Bus observer for HDR mode: subscribes to `PropertiesChanged`
/// on `org.kde.ScreenBrightness` displays. When the `Brightness`
/// property changes, looks up the display label in `label_to_tv`
/// (populated by reconcile) and forwards to the event channel.
async fn observe_kde_brightness_dbus(
    event_tx: mpsc::UnboundedSender<BrightnessEvent>,
    label_to_tv: Arc<Mutex<HashMap<String, String>>>,
) {
    use futures_util::StreamExt;

    let Ok(conn) = zbus::Connection::session().await else {
        warn!("D-Bus brightness observer: session bus unavailable");
        return;
    };

    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.kde.ScreenBrightness")
        .unwrap()
        .interface("org.freedesktop.DBus.Properties")
        .unwrap()
        .member("PropertiesChanged")
        .unwrap()
        .build();

    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, &conn, None).await else {
        warn!("D-Bus brightness observer: failed to subscribe");
        return;
    };

    // Cache: display object path → (label, max_brightness).
    let mut path_cache: HashMap<String, (String, i32)> = HashMap::new();

    debug!("D-Bus brightness observer started");

    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        let body = msg.body();

        // PropertiesChanged(interface: s, changed: a{sv}, invalidated: as)
        let Ok((iface, changed, _invalidated)): Result<(
            String,
            HashMap<String, zbus::zvariant::OwnedValue>,
            Vec<String>,
        ), _> = body.deserialize() else {
            continue;
        };

        if iface != "org.kde.ScreenBrightness.Display" {
            continue;
        }

        let Some(brightness_val) = changed.get("Brightness") else {
            continue;
        };
        let Ok(brightness) = <i32>::try_from(brightness_val) else {
            continue;
        };

        // Resolve label + max_brightness, caching per display path.
        let path = match msg.header().path() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let (label, max_brightness) = match path_cache.get(&path) {
            Some(cached) => cached.clone(),
            None => {
                let label = read_display_label(&conn, &path).await;
                let max = read_display_max_brightness(&conn, &path).await;
                match (label, max) {
                    (Some(l), Some(m)) if m > 0 => {
                        path_cache.insert(path.clone(), (l.clone(), m));
                        (l, m)
                    }
                    _ => continue,
                }
            }
        };

        // Map label → tv_id via the reconcile-maintained lookup table.
        let tv_id = label_to_tv.lock().expect("lock poisoned").get(&label).cloned();

        if let Some(tv_id) = tv_id {
            let level = ((brightness as f64 / max_brightness as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u32;
            trace!(tv_id, brightness, max_brightness, level, "D-Bus brightness change");
            let _ = event_tx.send(BrightnessEvent::Requested {
                tv_id,
                value: level,
            });
        }
    }
}

/// Read a property from a ScreenBrightness display object.
async fn read_display_label(conn: &zbus::Connection, path: &str) -> Option<String> {
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination("org.kde.ScreenBrightness")
        .ok()?
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;
    let iface =
        zbus::names::InterfaceName::from_static_str_unchecked("org.kde.ScreenBrightness.Display");
    let prop = zbus::names::PropertyName::from_static_str_unchecked("Label");
    String::try_from(proxy.get(iface, &prop).await.ok()?).ok()
}

async fn read_display_max_brightness(conn: &zbus::Connection, path: &str) -> Option<i32> {
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination("org.kde.ScreenBrightness")
        .ok()?
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;
    let iface =
        zbus::names::InterfaceName::from_static_str_unchecked("org.kde.ScreenBrightness.Display");
    let prop = zbus::names::PropertyName::from_static_str_unchecked("MaxBrightness");
    i32::try_from(proxy.get(iface, &prop).await.ok()?).ok()
}

/// Reconcile desired brightness devices with current state.
async fn reconcile(
    sync: &mut BrightnessSync,
    daemon: &DaemonClient,
    state: &Arc<RwLock<SessionState>>,
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
                if !matches!(
                    profile.sync_backlight,
                    tisvlay_core::SyncBacklight::Backlight
                ) {
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

    // Drop devices no longer desired.
    let current_ids: Vec<String> = sync.bindings.keys().cloned().collect();
    for tv_id in current_ids {
        if !desired.contains_key(&tv_id) {
            let _ = sync.ops_tx.send(DeviceOp::Destroy {
                tv_id: tv_id.clone(),
            });
            sync.bindings.remove(&tv_id);
            debug!(tv_id, "unregistered brightness device");
        }
    }

    // Create devices for new TVs.
    for (tv_id, d) in &desired {
        if sync.bindings.contains_key(tv_id) {
            continue;
        }
        let _ = sync.ops_tx.send(DeviceOp::Create {
            tv_id: tv_id.clone(),
            edid_base64: d.edid_base64.clone(),
            initial_brightness: d.backlight,
        });
        sync.bindings.insert(
            tv_id.clone(),
            BindingMeta {
                last_observed: Some(d.backlight),
                monitor_name: d.monitor_name.clone(),
            },
        );
        debug!(tv_id, "registered brightness device");
    }

    // Rebuild the label→tv_id map for the D-Bus observer.
    {
        let mut map = sync.label_to_tv.lock().expect("lock poisoned");
        map.clear();
        for (tv_id, meta) in &sync.bindings {
            if let Some(name) = &meta.monitor_name {
                map.insert(name.clone(), tv_id.clone());
            }
        }
    }

    debug!(
        "brightness reconcile: desired={} bound={}",
        desired.len(),
        sync.bindings.len()
    );
}

struct DesiredBrightness {
    edid_base64: String,
    backlight: u8,
    /// EDID monitor name for D-Bus label matching.
    monitor_name: Option<String>,
}

/// EDID data read from a DRM connector's sysfs entry.
struct ConnectorEdid {
    /// First 128 bytes base64-encoded (for the Wayland protocol).
    base64: String,
    /// Monitor name from EDID descriptor blocks (for D-Bus label matching).
    monitor_name: Option<String>,
}

/// Read EDID from the DRM connector matching the TV's desktop config.
/// Returns the base64-encoded first 128 bytes and the parsed monitor name.
fn read_edid_for_tv(
    tv: &tisvlay_core::TvInfo,
    state: &SessionState,
) -> Option<ConnectorEdid> {
    let connector = state
        .user_config
        .tv
        .iter()
        .find(|e| e.id == tv.id)?
        .desktop
        .as_ref()?
        .connector
        .as_deref()?;

    let drm_root = std::path::Path::new("/sys/class/drm");
    for entry in std::fs::read_dir(drm_root).ok()?.flatten() {
        let dir_name = entry.file_name();
        let dir_name = dir_name.to_string_lossy();
        let Some(entry_connector) = dir_name.split_once('-').map(|(_, rest)| rest) else {
            continue;
        };
        if entry_connector != connector {
            continue;
        }
        let edid_bytes = std::fs::read(entry.path().join("edid")).ok()?;
        if edid_bytes.len() < 128 {
            return None;
        }
        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&edid_bytes[..128]);
        let monitor_name = edidkit::Edid::parse(&edid_bytes)
            .ok()
            .and_then(|e| e.product().monitor_name.clone())
            .filter(|n| !n.is_empty());
        return Some(ConnectorEdid {
            base64,
            monitor_name,
        });
    }
    None
}
