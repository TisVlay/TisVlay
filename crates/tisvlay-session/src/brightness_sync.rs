//! Brightness sync — KDE's brightness slider drives `Tv1.Backlight`.
//!
//! For every managed TV whose effective user profile selects
//! `sync_backlight = "kernel"`, the session service registers a
//! `kde_external_brightness_device_v1` with KWin so the desktop
//! brightness slider controls the TV's OLED pixel brightness.
//!
//! # Two forward paths
//!
//! **SDR mode**: KWin routes slider changes through the brightness
//! device. Our Wayland client receives `requested_brightness` events.
//!
//! **HDR mode**: KWin uses the slider for tone-mapping and does not
//! call `setBrightness` on the device. A D-Bus observer on
//! `org.kde.ScreenBrightness` catches `PropertiesChanged` signals
//! instead, scaling from KWin's 0–max range to 0–100.
//!
//! Both paths feed into the same drain-loop.
//!
//! # Reverse path (TV → KDE slider)
//!
//! `StateWatcher` delivers `PropertiesChanged` from `Tv1.Backlight`.
//! We call `set_observed_brightness` on the Wayland device. In HDR
//! mode KWin ignores this (known limitation).

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
    kde_external_brightness_v1::{self, KdeExternalBrightnessV1},
};

const KDE_EXTERNAL_BRIGHTNESS_VERSION: u32 = 3;

// ── Events & shared state ──────────────────────────────────────

/// A brightness value the drain-loop should forward to the TV.
#[derive(Debug, Clone)]
pub enum BrightnessEvent {
    Requested { tv_id: String, value: u32 },
}

/// State shared between the Wayland thread, the D-Bus observer, and
/// the coordinator. Interior-mutable behind `Arc`.
struct Shared {
    /// Brightness events from any forward path → drain-loop.
    event_tx: mpsc::UnboundedSender<BrightnessEvent>,
    /// Wayland object id → tv_id (routing `requested_brightness`).
    device_tv_map: Mutex<HashMap<u32, String>>,
    /// Display label → tv_id (routing D-Bus `PropertiesChanged`).
    /// Rebuilt on every reconcile from EDID monitor names.
    label_to_tv: Mutex<HashMap<String, String>>,
    /// tv_id → (display D-Bus path, max_brightness). Set by the D-Bus
    /// observer when it first resolves a label; used by the reverse
    /// path to call `SetBrightness` on the correct display.
    tv_display_info: Mutex<HashMap<String, (String, i32)>>,
    /// Last brightness each forward path sent to the TV. The reverse
    /// path checks this to suppress its own echoes.
    last_forward_sent: Mutex<HashMap<String, u8>>,
    /// Last brightness the reverse path sent to PowerDevil. The D-Bus
    /// observer checks this to suppress its own echoes.
    last_reverse_sent: Mutex<HashMap<String, i32>>,
}

// ── Wayland dispatch boilerplate ───────────────────────────────

struct WaylandState {
    shared: Arc<Shared>,
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

// ── Coordinator ────────────────────────────────────────────────

enum DeviceOp {
    Create {
        tv_id: String,
        edid_base64: String,
        initial_brightness: u8,
    },
    Destroy {
        tv_id: String,
    },
    Shutdown,
}

/// Per-TV state tracked by the coordinator.
struct Binding {
    /// EDID monitor name for D-Bus label matching.
    monitor_name: Option<String>,
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
        label_to_tv: Mutex::new(HashMap::new()),
        tv_display_info: Mutex::new(HashMap::new()),
        last_forward_sent: Mutex::new(HashMap::new()),
        last_reverse_sent: Mutex::new(HashMap::new()),
    });

    let (ops_tx, ops_rx) = std::sync::mpsc::channel();

    // Session bus connection for the D-Bus observer and reverse path.
    // Separate from the daemon connection (which may be system bus).
    let session_conn = match zbus::Connection::session().await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            warn!("Session D-Bus unavailable, brightness sync disabled: {e}");
            return;
        }
    };

    // Wayland thread for kde-external-brightness-v1.
    let (version, wayland_thread) = match start_wayland_thread(shared.clone(), ops_rx) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("Wayland brightness sync unavailable: {e}");
            return;
        }
    };

    // Forward drain-loop (events → set_backlight).
    let fwd_shared = shared.clone();
    let forward_handle = tokio::spawn(forward_brightness_events(
        event_rx,
        daemon.clone(),
        fwd_shared,
    ));

    // D-Bus observer for HDR mode.
    let dbus_handle = tokio::spawn(observe_kde_brightness_dbus(
        shared.clone(),
        session_conn.clone(),
    ));

    let mut bindings: HashMap<String, Binding> = HashMap::new();

    // Initial reconcile.
    reconcile(&mut bindings, &ops_tx, version, &shared, &daemon, &state).await;

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
        // Collect the action, then process. D-Bus calls from inside
        // the watcher arm can deadlock the shared zbus connection.
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
                info!("Brightness sync shutdown signalled");
                break;
            }
            Action::Tick => {
                reconcile(&mut bindings, &ops_tx, version, &shared, &daemon, &state).await;
            }
            Action::Props(tv_id) => {
                // Reverse path: read backlight from the daemon, then
                // push to PowerDevil via D-Bus SetBrightness (not the
                // Wayland set_observed_brightness, which is one-shot).
                if let Ok(tvs) = daemon.list_tvs().await {
                    if let Some(tv) = tvs.iter().find(|t| t.id == tv_id) {
                        let backlight = tv.backlight;
                        // Suppress echo from our own forward writes.
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
                            push_brightness_to_kde(&shared, &session_conn, &tv_id, backlight).await;
                        }
                    }
                }
                reconcile(&mut bindings, &ops_tx, version, &shared, &daemon, &state).await;
            }
            Action::StreamClosed => {
                warn!("StateWatcher stream closed; exiting brightness sync");
                break;
            }
        }
    }

    // Teardown.
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

// ── Wayland thread ─────────────────────────────────────────────

fn start_wayland_thread(
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
        // Drain ops from the tokio side.
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

        // Non-blocking Wayland dispatch.
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

// ── Forward drain-loop ─────────────────────────────────────────

async fn forward_brightness_events(
    mut rx: mpsc::UnboundedReceiver<BrightnessEvent>,
    daemon: Arc<DaemonClient>,
    shared: Arc<Shared>,
) {
    while let Some(first) = rx.recv().await {
        // Coalesce burst: take the latest value per TV.
        let mut latest: HashMap<String, u32> = HashMap::new();
        let BrightnessEvent::Requested { tv_id, value } = &first;
        latest.insert(tv_id.clone(), *value);
        while let Ok(next) = rx.try_recv() {
            let BrightnessEvent::Requested { tv_id, value } = next;
            latest.insert(tv_id, value);
        }
        for (tv_id, value) in latest {
            let level = (value as u8).min(100);
            shared
                .last_forward_sent
                .lock()
                .expect("poisoned")
                .insert(tv_id.clone(), level);
            if let Err(e) = daemon.set_backlight(&tv_id, level).await {
                warn!(tv_id, level, error = %e, "forward brightness set failed");
            }
        }
    }
}

// ── D-Bus observer (HDR fallback) ──────────────────────────────

/// Subscribe to `PropertiesChanged` on `org.kde.ScreenBrightness`
/// displays. Fires in both SDR and HDR modes; in SDR the Wayland
/// path also fires and the drain-loop deduplicates.
async fn observe_kde_brightness_dbus(shared: Arc<Shared>, conn: Arc<zbus::Connection>) {
    use futures_util::StreamExt;

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

    let mut path_cache: HashMap<String, (String, i32)> = HashMap::new();
    debug!("D-Bus brightness observer started");

    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };

        let Ok((iface, changed, _)): Result<
            (
                String,
                HashMap<String, zbus::zvariant::OwnedValue>,
                Vec<String>,
            ),
            _,
        > = msg.body().deserialize() else {
            continue;
        };

        if iface != "org.kde.ScreenBrightness.Display" {
            continue;
        }
        let Some(val) = changed.get("Brightness") else {
            continue;
        };
        let Ok(brightness) = i32::try_from(val) else {
            continue;
        };

        let path = match msg.header().path() {
            Some(p) => p.to_string(),
            None => continue,
        };

        let (label, max) = match path_cache.get(&path) {
            Some(c) => c.clone(),
            None => {
                let l = read_display_prop::<String>(&conn, &path, "Label").await;
                let m = read_display_prop::<i32>(&conn, &path, "MaxBrightness").await;
                match (l, m) {
                    (Some(l), Some(m)) if m > 0 => {
                        path_cache.insert(path.clone(), (l.clone(), m));
                        (l, m)
                    }
                    _ => continue,
                }
            }
        };

        let tv_id = shared
            .label_to_tv
            .lock()
            .expect("poisoned")
            .get(&label)
            .cloned();
        if let Some(tv_id) = tv_id {
            // Store display info for the reverse path.
            shared
                .tv_display_info
                .lock()
                .expect("poisoned")
                .insert(tv_id.clone(), (path.clone(), max));

            // Suppress echo from our own reverse-path writes.
            {
                let mut sent = shared.last_reverse_sent.lock().expect("poisoned");
                if sent.get(&tv_id) == Some(&brightness) {
                    sent.remove(&tv_id);
                    continue;
                }
            }

            let level = ((brightness as f64 / max as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u32;
            trace!(tv_id, brightness, max, level, "D-Bus brightness change");
            let _ = shared.event_tx.send(BrightnessEvent::Requested {
                tv_id,
                value: level,
            });
        }
    }
}

/// Push a TV backlight value to PowerDevil's D-Bus display via
/// `SetBrightness`. Scales from 0-100 to the display's max range.
async fn push_brightness_to_kde(
    shared: &Shared,
    conn: &zbus::Connection,
    tv_id: &str,
    backlight: u8,
) {
    let (path, max) = {
        let info = shared.tv_display_info.lock().expect("poisoned");
        match info.get(tv_id) {
            Some((p, m)) => (p.clone(), *m),
            None => return, // D-Bus observer hasn't resolved this TV yet
        }
    };

    // Scale 0-100 → 0-max.
    let kde_value = ((backlight as f64 / 100.0) * max as f64)
        .round()
        .clamp(0.0, max as f64) as i32;

    // Record for echo suppression before sending.
    shared
        .last_reverse_sent
        .lock()
        .expect("poisoned")
        .insert(tv_id.to_string(), kde_value);

    let result = conn
        .call_method(
            Some("org.kde.ScreenBrightness"),
            &*path,
            Some("org.kde.ScreenBrightness.Display"),
            "SetBrightness",
            &(kde_value, 0u32),
        )
        .await;

    match result {
        Ok(_) => trace!(
            tv_id,
            backlight, kde_value, "reverse: pushed brightness to PowerDevil"
        ),
        Err(e) => warn!(tv_id, error = %e, "reverse: SetBrightness failed"),
    }
}

async fn read_display_prop<T: TryFrom<zbus::zvariant::OwnedValue>>(
    conn: &zbus::Connection,
    path: &str,
    property: &'static str,
) -> Option<T> {
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
    let prop = zbus::names::PropertyName::from_static_str_unchecked(property);
    T::try_from(proxy.get(iface, &prop).await.ok()?).ok()
}

// ── Reconcile ──────────────────────────────────────────────────

async fn reconcile(
    bindings: &mut HashMap<String, Binding>,
    ops_tx: &std::sync::mpsc::Sender<DeviceOp>,
    _version: u32,
    shared: &Arc<Shared>,
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
                if !matches!(profile.sync_backlight, tisvlay_core::SyncBacklight::Kernel) {
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

    // Drop stale bindings.
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

    // Create new bindings.
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

    // Rebuild label→tv_id map for the D-Bus observer.
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

// ── Helpers ────────────────────────────────────────────────────

struct DesiredBrightness {
    edid_base64: String,
    backlight: u8,
    monitor_name: Option<String>,
}

struct ConnectorEdid {
    base64: String,
    monitor_name: Option<String>,
}

/// Read EDID from the DRM connector matching the TV's desktop config.
fn read_edid_for_tv(tv: &tisvlay_core::TvInfo, state: &SessionState) -> Option<ConnectorEdid> {
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
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(entry_connector) = name.split_once('-').map(|(_, r)| r) else {
            continue;
        };
        if entry_connector != connector {
            continue;
        }

        let bytes = std::fs::read(entry.path().join("edid")).ok()?;
        if bytes.len() < 128 {
            return None;
        }

        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes[..128]);
        let monitor_name = edidkit::Edid::parse(&bytes)
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
