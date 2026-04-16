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
use zbus;
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
    version: u32,
    last_observed: Option<u8>,
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

/// Handle to the Wayland connection for creating/destroying devices.
/// Lives on the tokio side; all Wayland calls are dispatched via the
/// event queue on the dedicated Wayland thread.
struct BrightnessSync {
    /// Pending device operations queued from tokio, consumed by the
    /// Wayland thread on its next dispatch cycle.
    ops_tx: std::sync::mpsc::Sender<DeviceOp>,
    /// Negotiated protocol version.
    version: u32,
    /// Tracks which TVs have active devices and their protocol version.
    bindings: HashMap<String, BindingMeta>,
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

    let forward_handle = tokio::spawn(forward_brightness_events(event_rx, daemon.clone()));

    // D-Bus observer for HDR mode: subscribe to
    // org.kde.ScreenBrightness.BrightnessChanged which fires even
    // when KWin doesn't route through the brightness device.
    let dbus_observer_handle = tokio::spawn(observe_kde_brightness_dbus(
        shared.event_tx.clone(),
        state.clone(),
    ));

    let mut sync = BrightnessSync {
        ops_tx,
        version,
        bindings: HashMap::new(),
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
        tokio::select! {
            _ = shutdown.changed() => {
                info!("Brightness sync shutdown signalled");
                break;
            }
            _ = tick.tick() => {
                reconcile(&mut sync, &daemon, &state).await;
            }
            evt = watcher.next() => {
                match evt {
                    Some(StateEvent::DaemonStateChanged(TvStateChange::PropertiesChanged { tv_id })) => {
                        push_observed_brightness(&mut sync, &daemon, &tv_id).await;
                        reconcile(&mut sync, &daemon, &state).await;
                    }
                    Some(StateEvent::DaemonStateChanged(_))
                    | Some(StateEvent::FallbackPoll)
                    | Some(StateEvent::DaemonAppeared)
                    | Some(StateEvent::DaemonDisappeared)
                    | Some(StateEvent::SessionProfileChanged) => {
                        reconcile(&mut sync, &daemon, &state).await;
                    }
                    Some(StateEvent::SessionAppeared | StateEvent::SessionDisappeared) => {}
                    None => {
                        warn!("StateWatcher stream closed; exiting brightness sync");
                        break;
                    }
                }
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
async fn push_observed_brightness(
    sync: &mut BrightnessSync,
    daemon: &DaemonClient,
    tv_id: &str,
) {
    let Some(meta) = sync.bindings.get_mut(tv_id) else {
        return;
    };
    if meta.version < 2 {
        return;
    }
    let backlight = match daemon.get_backlight(tv_id).await {
        Ok(v) => v,
        Err(_) => return,
    };
    if meta.last_observed == Some(backlight) {
        return;
    }
    meta.last_observed = Some(backlight);
    let _ = sync.ops_tx.send(DeviceOp::SetObservedBrightness {
        tv_id: tv_id.to_string(),
        value: backlight,
    });
    trace!(tv_id, backlight, "pushed observed brightness to compositor");
}

/// Forward brightness events from KDE to the TV via the daemon.
async fn forward_brightness_events(
    mut rx: mpsc::UnboundedReceiver<BrightnessEvent>,
    daemon: Arc<DaemonClient>,
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
            if let Err(e) = daemon.set_backlight(&tv_id, level).await {
                warn!(tv_id, level, error = %e, "forward brightness set failed");
            }
        }
    }
}

/// Observe `org.kde.ScreenBrightness.BrightnessChanged` on the session
/// D-Bus. This signal fires in both SDR and HDR modes — when the
/// Wayland `requested_brightness` path is active (SDR), both fire and
/// the drain-loop deduplicates; when only D-Bus fires (HDR), this is
/// the sole forward path.
///
/// The signal signature is `(siss)`: display_name, brightness,
/// max_brightness, source_client_name.
async fn observe_kde_brightness_dbus(
    event_tx: mpsc::UnboundedSender<BrightnessEvent>,
    state: Arc<RwLock<SessionState>>,
) {
    use futures_util::StreamExt;

    let Ok(conn) = zbus::Connection::session().await else {
        warn!("D-Bus brightness observer: session bus unavailable");
        return;
    };

    // Subscribe to PropertiesChanged on every ScreenBrightness display.
    // This is more reliable than matching the custom BrightnessChanged
    // signal and works with zbus's standard proxy machinery.
    // Subscribe to BrightnessChanged via zbus proxy for
    // org.freedesktop.DBus.Properties.PropertiesChanged on each display.
    // We use a simpler approach: monitor PropertiesChanged on
    // org.kde.ScreenBrightness.Display and extract the Brightness value.
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

    debug!("D-Bus brightness observer started");

    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        debug!(
            "D-Bus observer: signal from {:?} {:?}.{:?}",
            msg.header().path(),
            msg.header().interface(),
            msg.header().member(),
        );
        let body = msg.body();

        // PropertiesChanged(interface: s, changed: a{sv}, invalidated: as)
        let Ok((iface, changed, _invalidated)): Result<(
            String,
            HashMap<String, zbus::zvariant::OwnedValue>,
            Vec<String>,
        ), _> = body.deserialize() else {
            debug!("D-Bus observer: failed to deserialize PropertiesChanged");
            continue;
        };

        if iface != "org.kde.ScreenBrightness.Display" {
            debug!("D-Bus observer: ignoring interface {iface}");
            continue;
        }

        let Some(brightness_val) = changed.get("Brightness") else {
            debug!("D-Bus observer: no Brightness in changed props, keys={:?}", changed.keys().collect::<Vec<_>>());
            continue;
        };
        let Ok(brightness) = <i32>::try_from(brightness_val) else {
            debug!("D-Bus observer: Brightness value not i32: {:?}", brightness_val);
            continue;
        };
        debug!("D-Bus observer: brightness={brightness} from PropertiesChanged");

        // Get the display label to map to a TV ID. Read it from the
        // same object path the signal came from.
        let display_path = msg.header().path().map(|p| p.to_string());
        let display_label = if let Some(ref path) = display_path {
            let proxy = zbus::fdo::PropertiesProxy::builder(&conn)
                .destination("org.kde.ScreenBrightness")
                .unwrap()
                .path(path.as_str())
                .unwrap()
                .build()
                .await
                .ok();
            if let Some(proxy) = proxy {
                let iface_name = zbus::names::InterfaceName::from_static_str_unchecked(
                    "org.kde.ScreenBrightness.Display",
                );
                let prop_name =
                    zbus::names::PropertyName::from_static_str_unchecked("Label");
                proxy
                    .get(iface_name, &prop_name)
                    .await
                    .ok()
                    .and_then(|v| <String>::try_from(v).ok())
            } else {
                None
            }
        } else {
            None
        };

        let Some(display_name) = display_label else {
            debug!("D-Bus observer: label lookup failed for {:?}", display_path);
            continue;
        };
        debug!("D-Bus observer: label={display_name}");

        // KWin uses 0-10000 for software brightness. Scale to 0-100.
        // TODO: read MaxBrightness from the display's D-Bus property
        // for exactness; 10000 is KWin's hardcoded default.
        let max_brightness = 10000i32;
        let level = ((brightness as f64 / max_brightness as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u32;

        // Map the KDE display label to a TV ID.
        let tv_id = {
            let s = state.read().await;
            find_tv_for_display_label(&display_name, &s)
        };

        if let Some(tv_id) = tv_id {
            trace!(
                tv_id,
                display_name,
                brightness,
                level,
                "D-Bus brightness change (HDR path)"
            );
            let _ = event_tx.send(BrightnessEvent::Requested {
                tv_id,
                value: level,
            });
        }
    }
}

/// Match a KDE display label to a TV ID. The label comes from KWin's
/// KScreen output name, which is typically the EDID monitor name
/// (e.g. "LG TV SSCR2"). We compare against the TV's HardwareId
/// model field, which is populated from the same EDID source.
fn find_tv_for_display_label(label: &str, state: &SessionState) -> Option<String> {
    // Check each TV that has brightness sync enabled.
    for (tv_id, profile) in &state.profiles {
        if !matches!(
            profile.sync_backlight,
            tisvlay_core::SyncBacklight::Backlight
        ) {
            continue;
        }
        // Look up the TV's desktop config for the connector, then
        // read the ELD monitor name from sysfs to match against the
        // KDE label. For simplicity, match against the desktop mapping's
        // label if available, or fall back to a substring match on
        // the TV's hardware_id model.
        if let Some(tv_entry) = state.user_config.tv.iter().find(|e| e.id == *tv_id) {
            if let Some(desktop) = &tv_entry.desktop {
                if let Some(connector) = &desktop.connector {
                    // Read the EDID monitor name from the DRM connector
                    let monitor_name = read_monitor_name(connector);
                    if let Some(name) = monitor_name {
                        if label == name {
                            return Some(tv_id.clone());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Read the monitor name from a DRM connector's EDID status file.
fn read_monitor_name(connector: &str) -> Option<String> {
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
        let edid_path = entry.path().join("edid");
        let edid_bytes = std::fs::read(&edid_path).ok()?;
        if edid_bytes.len() < 128 {
            return None;
        }
        // Parse EDID to extract monitor name from descriptor blocks
        if let Ok(edid) = edidkit::Edid::parse(&edid_bytes) {
            let name = edid.product().monitor_name.clone();
            return name.filter(|n| !n.is_empty());
        }
    }
    None
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
                // Need the EDID for the brightness device registration.
                let edid = read_edid_for_tv(tv, &s)?;
                Some((
                    tv.id.clone(),
                    DesiredBrightness {
                        edid_base64: edid,
                        backlight: tv.backlight,
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
                version: sync.version,
                last_observed: Some(d.backlight),
            },
        );
        debug!(tv_id, "registered brightness device");
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
}

/// Read raw EDID bytes for a TV from sysfs and base64-encode the first
/// 128 bytes (what the protocol expects).
fn read_edid_for_tv(
    tv: &tisvlay_core::TvInfo,
    state: &SessionState,
) -> Option<String> {
    // Look up the connector from the user config's desktop mapping.
    let connector = state
        .user_config
        .tv
        .iter()
        .find(|e| e.id == tv.id)?
        .desktop
        .as_ref()?
        .connector
        .as_deref()?;

    // Find the sysfs EDID for this connector by scanning DRM.
    let drm_root = std::path::Path::new("/sys/class/drm");
    let entries = std::fs::read_dir(drm_root).ok()?;
    for entry in entries.flatten() {
        let dir_name = entry.file_name();
        let dir_name = dir_name.to_string_lossy();
        let Some(entry_connector) = dir_name.split_once('-').map(|(_, rest)| rest) else {
            continue;
        };
        if entry_connector != connector {
            continue;
        }
        let edid_path = entry.path().join("edid");
        let edid = std::fs::read(&edid_path).ok()?;
        if edid.len() >= 128 {
            use base64::Engine;
            return Some(base64::engine::general_purpose::STANDARD.encode(&edid[..128]));
        }
    }
    None
}
