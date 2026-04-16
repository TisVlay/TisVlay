//! Volume sync — KDE's native HDMI volume widget drives the TV.
//!
//! For every managed TV whose effective user profile selects
//! `sync_volume = "mirror_hdmi"` and whose backend advertises
//! `Capabilities::VOLUME`, the session service binds to the existing
//! PipeWire HDMI sink that corresponds to the TV and bridges
//! `Tv1.Volume` with the sink's `Props.channelVolumes` / `Props.mute`.
//! No separate virtual sink is created — the TV's native row in the
//! audio widget is the one the user scrolls.
//!
//! # Identifying the right sink
//!
//! Each ALSA HDMI PCM has an ELD file under `/proc/asound/card$N/eld#*`
//! that carries the connected display's EDID-like data (PNPID,
//! monitor name). The filename uses HDA-internal codec / pin indices
//! rather than the ALSA PCM `device` number PipeWire reports, so
//! there's no direct mapping from a sink's props to its ELD file —
//! instead we scan every ELD on the sink's ALSA card, keep the ones
//! with `monitor_present=1`, and pick the one whose `monitor_name`
//! matches the sink's `node.nick` (PipeWire populates `node.nick`
//! from the same ELD string). The resulting [`HardwareId`] is the
//! same shape used by the `desktop_probe` module on the DRM side,
//! so [`crate::desktop_probe::identity_matches`] /
//! [`crate::desktop_probe::match_score`] carry over for matching —
//! including the EDID quirks we already tolerate (generic LG
//! `monitor_name`, empty serial field).
//!
//! Registry globals only expose a minimal prop subset (no `alsa.*`
//! keys), so the full prop set required for ELD lookup only arrives
//! via the Node's `info` event. We therefore `Registry::bind` every
//! Audio/Sink on discovery to trigger `info`, and keep the Node
//! proxy for later `set_param` / Props-listener use.
//!
//! # Runtime model
//!
//! `pipewire-native` is synchronous and runs its own C event loop on
//! an internal thread ([`ThreadLoop`]). All PipeWire state lives on
//! that thread. From the tokio side we serialise PipeWire operations
//! behind `thread_loop.lock()` (brief critical sections). Callbacks
//! fire on the PipeWire thread and post into a
//! [`tokio::sync::mpsc::UnboundedSender`] that the coordinator drains.
//!
//! # Feedback-loop suppression
//!
//! When the TV reports a new volume, we push it to the sink via
//! `Node::set_param(Props)`. The sink's `param` listener then fires,
//! which would otherwise cause us to immediately write it back to
//! the TV. A last-pushed cache per TV discards listener events whose
//! values match.
//!
//! # Volume scale
//!
//! PipeWire's `channelVolumes` is cubic (`cv^3` is the actual gain),
//! and Plasma displays percent as `cbrt(cv) * 100`. Our host-side
//! `u8` Volume (0-100) is a *linear* percent; we cube on write and
//! cube-root on read so the displayed percentage matches the TV.
//!
//! # Lifecycle
//!
//! Bindings are created/dropped by the coordinator every 10 s and on
//! every `StateWatcher` event: TV connected + profile says
//! `"mirror_hdmi"` + a candidate sink whose ELD matches → bind; TV
//! disconnected / profile flips to `"none"` / no matching sink →
//! unbind. We never destroy the sink (we don't own it).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use pipewire_native::{
    context::Context as PwContext,
    core::Core,
    properties::Properties,
    proxy::device::{Device, DeviceEvents},
    proxy::node::{Node, NodeEvents},
    proxy::registry::{Registry, RegistryEvents},
    thread_loop::ThreadLoop,
    types,
};
use pipewire_native_spa as spa;
use spa::param::ParamType;
use spa::param::props::Prop;
use spa::param::route::Route;
use spa::pod::types::{ObjectType, PropertyFlags};
use tisvlay_core::{
    DaemonClient, HardwareId, StateEvent, StateWatcher, StateWatcherConfig, TvStateChange,
};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, instrument, trace, warn};

use crate::SessionState;
use crate::desktop_probe::{identity_matches, match_score};

/// `channelVolumes` value corresponding to 0 dB (unity) gain. Native
/// HDMI sinks use 1.0 here; matches what Plasma writes.
const PW_UNITY_VOLUME: f32 = 1.0;

/// Events emitted from the PipeWire thread into the coordinator —
/// user-initiated sink changes that need to reach the TV.
#[derive(Debug, Clone)]
pub enum SinkEvent {
    VolumeChanged { tv_id: String, volume: u8 },
    MuteChanged { tv_id: String, muted: bool },
}

/// Owns the PipeWire connection, the registry cache of candidate
/// HDMI sinks, and the current set of bindings. Cloneable: internal
/// state is refcounted. Drop the last clone to release every binding
/// and quit the [`ThreadLoop`].
#[derive(Clone)]
pub struct VolumeSync {
    inner: Arc<Inner>,
}

struct Inner {
    thread_loop: ThreadLoop,
    /// Context must outlive `Core` — `Core` holds a `Weak<Context>`.
    _pw_context: PwContext,
    /// `Registry` already keeps `Core` alive via an internal clone;
    /// we hold it here only to guarantee ordering at drop time
    /// (Registry drops before Core, Core before Context).
    _core: Core,
    registry: Registry,
    /// Every Audio/Sink global we've seen, keyed by global id.
    /// Populated from the registry listener.
    candidates: Mutex<HashMap<u32, SinkCandidate>>,
    /// Bound Device proxies, keyed by device global id. Used for
    /// the reverse path (TV → sink) via `Device::set_param(Route)`.
    devices: Mutex<HashMap<u32, DeviceState>>,
    /// Bindings indexed by `tv_id`.
    bindings: Mutex<HashMap<String, Binding>>,
    event_tx: mpsc::UnboundedSender<SinkEvent>,
}

/// A PipeWire Device we've bound to for route-level volume control.
struct DeviceState {
    device: Device,
    /// Active routes keyed by the route's `device` index (the
    /// internal index within the Device, NOT the global id). Each
    /// entry records the `Route::Index` needed for `set_param`.
    routes: HashMap<u32, u32>,
}

/// A PipeWire Audio/Sink we've bound to. Held in
/// [`Inner::candidates`] keyed by the server's global id; see the
/// module docs for why we bind on discovery rather than lazily.
struct SinkCandidate {
    /// Refcounted Node proxy. Dropped when the global is removed.
    node: Node,
    /// Populated after the Node's first `info` event. `None` for
    /// non-ALSA sinks (Bluetooth, analog, etc.) and for sinks we
    /// haven't received info for yet.
    hardware_id: Option<HardwareId>,
    /// ELD monitor_name from the latest `info` props — kept for
    /// status output / tray readouts.
    #[allow(dead_code)]
    nick: Option<String>,
    /// PipeWire Device global id that owns this node (`device.id`
    /// from the node's info props). Needed to look up the Device
    /// proxy for route-level volume writes.
    device_global_id: Option<u32>,
    /// The `card.profile.device` index — identifies which route on
    /// the Device corresponds to this node.
    route_device_index: Option<u32>,
}

struct Binding {
    /// Which candidate we bound against. The proxy lives in
    /// [`SinkCandidate`]; the binding records suppression state and
    /// the listener hook so we can detach on unbind.
    global_id: u32,
    hook_id: pipewire_native::HookId,
    last_pushed_volume: Option<u8>,
    last_pushed_muted: Option<bool>,
}

impl VolumeSync {
    /// Initialise PipeWire, start the thread loop, and subscribe to
    /// registry globals. Returns the handle and a receiver of
    /// [`SinkEvent`]s.
    pub fn connect() -> Result<(Self, mpsc::UnboundedReceiver<SinkEvent>)> {
        // `pipewire-native` looks for SPA plugins and config files
        // under paths baked in at build time. On NixOS those live
        // inside the Nix store, so the defaults miss. Auto-detect by
        // walking `/proc` for the running PipeWire daemon and
        // deriving the paths relative to its exe.
        ensure_pipewire_paths();

        pipewire_native::init();

        let thread_loop = ThreadLoop::new(&Properties::new())
            .ok_or_else(|| anyhow!("pipewire-native failed to create ThreadLoop"))?;
        let pw_context = PwContext::new(thread_loop.main_loop(), Properties::new())
            .context("pipewire-native Context::new")?;

        thread_loop.run();

        // Build Inner + attach the registry listener while the
        // ThreadLoop is locked — otherwise the PW thread runs the
        // initial burst of `global` events before our listener is
        // attached, and every existing sink goes unseen.
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let inner = {
            let _g = thread_loop.lock();
            let core = pw_context
                .connect(None)
                .context("pipewire-native Core::connect")?;
            let registry = core.registry().context("Core::registry")?;

            let inner = Arc::new(Inner {
                thread_loop: thread_loop.clone(),
                _pw_context: pw_context.clone(),
                _core: core,
                registry,
                candidates: Mutex::new(HashMap::new()),
                devices: Mutex::new(HashMap::new()),
                bindings: Mutex::new(HashMap::new()),
                event_tx,
            });

            let inner_for_global = Arc::clone(&inner);
            let inner_for_remove = Arc::clone(&inner);
            inner.registry.add_listener(RegistryEvents {
                global: Some(Box::new(
                    move |id, _perms, type_, _version, props: &Properties| {
                        if type_ != types::interface::NODE {
                            return;
                        }
                        handle_global_added(&inner_for_global, id, props);
                    },
                )),
                global_remove: Some(Box::new(move |id| {
                    handle_global_removed(&inner_for_remove, id);
                })),
            });
            inner
        };

        info!("PipeWire connection established for volume sync");
        Ok((Self { inner }, event_rx))
    }

    /// Snapshot of the candidate sinks whose ELD resolved to a
    /// non-empty `HardwareId`. Used by the coordinator to compute
    /// bindings against its TV list. Returns only the data the
    /// matcher needs so `SinkCandidate` (which holds a `Node`) stays
    /// private.
    fn candidate_identities(&self) -> Vec<(u32, HardwareId)> {
        self.inner
            .candidates
            .lock()
            .expect("candidates lock poisoned")
            .iter()
            .filter_map(|(id, c)| c.hardware_id.clone().map(|hw| (*id, hw)))
            .collect()
    }

    /// Bind to the PipeWire sink with the given `global_id` and
    /// attach a Props listener for the forward path. Idempotent: if
    /// we're already bound to this id we reuse the binding; if we
    /// were bound to a different id for this TV, the previous
    /// binding is dropped first.
    #[instrument(skip(self))]
    pub fn bind_tv(
        &self,
        tv_id: &str,
        global_id: u32,
        initial_volume: u8,
        initial_muted: bool,
    ) -> Result<()> {
        let needs_rebind = {
            let bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
            match bindings.get(tv_id) {
                Some(b) if b.global_id == global_id => false,
                Some(_) => true, // different sink — rebind
                None => true,
            }
        };
        if !needs_rebind {
            return Ok(());
        }
        // Drop any stale binding first (releases the Props listener
        // from the previous sink).
        self.unbind_tv(tv_id);

        // Clone the already-bound Node from the candidate map so we
        // don't hold the outer lock across the PipeWire call.
        let node = {
            let candidates = self
                .inner
                .candidates
                .lock()
                .expect("candidates lock poisoned");
            let Some(c) = candidates.get(&global_id) else {
                return Err(anyhow!("no candidate sink for global_id {global_id}"));
            };
            c.node.clone()
        };

        let hook_id = {
            let _g = self.inner.thread_loop.lock();
            node.subscribe_params(&[ParamType::Props])
                .context("Node::subscribe_params(Props) failed")?;

            let event_tx = self.inner.event_tx.clone();
            let tv_id_for_cb = tv_id.to_string();
            let inner_for_cb = Arc::clone(&self.inner);
            node.add_listener(NodeEvents {
                param: Some(Box::new(
                    move |_seq, param_id, _index, _next, pod: &spa::pod::RawPodOwned| {
                        if param_id != ParamType::Props {
                            return;
                        }
                        handle_props_change(&inner_for_cb, &tv_id_for_cb, &event_tx, pod);
                    },
                )),
                ..Default::default()
            })
        };

        self.inner
            .bindings
            .lock()
            .expect("bindings lock poisoned")
            .insert(
                tv_id.to_string(),
                Binding {
                    global_id,
                    hook_id,
                    last_pushed_volume: Some(initial_volume),
                    last_pushed_muted: Some(initial_muted),
                },
            );

        // Seed the sink with the TV's current state so the desktop
        // slider starts where the TV actually is. Errors here are
        // non-fatal — next reconcile will retry.
        if let Err(e) = self.push_tv_state(tv_id, initial_volume, initial_muted) {
            warn!(tv_id, error = %e, "failed to seed initial volume");
        }

        info!(tv_id, global_id, "bound TV volume to HDMI sink");
        Ok(())
    }

    /// Drop the binding for `tv_id` if present. Detaches the Props
    /// listener from the shared candidate Node. We don't destroy the
    /// server-side sink — it's not ours to destroy. Idempotent.
    #[instrument(skip(self))]
    pub fn unbind_tv(&self, tv_id: &str) {
        let prior = {
            let mut bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
            bindings.remove(tv_id)
        };
        if let Some(prior) = prior {
            // Remove the param listener from the candidate's Node so
            // future Props events don't fire into a dropped tv_id
            // context. Hook removal is cheap and safe if the
            // candidate has since disappeared from the map.
            let node = {
                let candidates = self
                    .inner
                    .candidates
                    .lock()
                    .expect("candidates lock poisoned");
                candidates.get(&prior.global_id).map(|c| c.node.clone())
            };
            if let Some(node) = node {
                let _g = self.inner.thread_loop.lock();
                node.remove_listener(prior.hook_id);
            }
            info!(tv_id, "unbound TV volume from HDMI sink");
        }
    }

    /// Push TV-side state to the bound sink (reverse path). Updates
    /// `Props.channelVolumes` + `Props.mute` and records the values
    /// for feedback-loop suppression.
    #[instrument(skip(self))]
    pub fn push_tv_state(&self, tv_id: &str, volume: u8, muted: bool) -> Result<()> {
        let (device_global_id, route_device_index) = {
            let bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
            let Some(b) = bindings.get(tv_id) else {
                return Err(anyhow!("no binding for {tv_id}"));
            };
            let candidates = self
                .inner
                .candidates
                .lock()
                .expect("candidates lock poisoned");
            let Some(c) = candidates.get(&b.global_id) else {
                return Err(anyhow!(
                    "candidate sink for binding (global_id {}) disappeared",
                    b.global_id
                ));
            };
            let dev_gid = c
                .device_global_id
                .ok_or_else(|| anyhow!("node has no device.id yet"))?;
            let route_dev = c
                .route_device_index
                .ok_or_else(|| anyhow!("node has no card.profile.device yet"))?;
            (dev_gid, route_dev)
        };

        let (device, route_index) = {
            let devices = self.inner.devices.lock().expect("devices lock poisoned");
            let ds = devices
                .get(&device_global_id)
                .ok_or_else(|| anyhow!("Device {device_global_id} not bound"))?;
            let ri = ds
                .routes
                .get(&route_device_index)
                .copied()
                .ok_or_else(|| {
                    anyhow!(
                        "no route for device_index {route_device_index} on Device {device_global_id}"
                    )
                })?;
            (ds.device.clone(), ri)
        };

        let volume_pw = u8_to_pw_volume(volume);
        let channel_volumes = vec![volume_pw, volume_pw];

        debug!(
            tv_id,
            volume,
            muted,
            volume_pw,
            device_global_id,
            route_index,
            route_device_index,
            "push_tv_state: writing Device Route"
        );

        // Build the nested Props object as a standalone pod. The
        // ObjectBuilder API doesn't support nesting objects inside
        // properties, so we pre-serialise and push as a RawPodOwned.
        let props_pod = build_props_pod(channel_volumes, muted)?;

        {
            let _g = self.inner.thread_loop.lock();
            device
                .set_param(
                    ParamType::Route,
                    ObjectType::ParamRoute,
                    0,
                    Box::new(move |builder| {
                        builder
                            .push_property(
                                Route::Index,
                                PropertyFlags::empty(),
                                route_index as i32,
                            )
                            .push_property(
                                Route::Device,
                                PropertyFlags::empty(),
                                route_device_index as i32,
                            )
                            .push_property(Route::Props, PropertyFlags::empty(), props_pod)
                            .push_property(Route::Save, PropertyFlags::empty(), true)
                    }),
                )
                .context("Device::set_param(Route)")?;
        }

        let mut bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
        if let Some(b) = bindings.get_mut(tv_id) {
            b.last_pushed_volume = Some(volume);
            b.last_pushed_muted = Some(muted);
        }
        Ok(())
    }

    /// Currently-bound TV ids. For tests and status readouts.
    pub fn bound_tvs(&self) -> Vec<String> {
        self.inner
            .bindings
            .lock()
            .expect("bindings lock poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for VolumeSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VolumeSync")
            .field("bound_tvs", &self.bound_tvs())
            .finish_non_exhaustive()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Candidate Nodes (and their listeners / hooks) drop with
        // the field, so we only need to stop the PipeWire thread.
        self.thread_loop.quit();
        info!("PipeWire volume sync shut down");
    }
}

/// Registry callback: a new global appeared. If it's an Audio/Sink
/// with ALSA HDMI props we try to parse its ELD and add it to the
/// candidate set.
fn handle_global_added(inner: &Arc<Inner>, id: u32, props: &Properties) {
    let Some(media_class) = props.get("media.class") else {
        return;
    };
    if media_class != "Audio/Sink" {
        return;
    }
    let node_name = props.get("node.name").unwrap_or("").to_string();
    debug!("Audio/Sink global seen: id={id} node_name={node_name:?}");

    // Bind the node so we can receive its `info` event and derive a
    // HardwareId from the full prop set (registry globals only carry
    // a minimal subset — no `alsa.*` keys). Keep the proxy around;
    // a later `bind_tv` attaches a Props listener for the forward
    // path and calls `set_param` for the reverse path on this same
    // proxy.
    //
    // We're currently *on* the PipeWire thread (this runs inside the
    // registry global callback), so the ThreadLoop's own
    // before/after hooks have already acquired its mutex. Trying to
    // `thread_loop.lock()` here would self-deadlock — call PipeWire
    // methods directly.
    let object = match inner.registry.bind(id, types::interface::NODE, 3) {
        Ok(o) => o,
        Err(e) => {
            warn!("Registry::bind({id}) failed: {e:#}");
            return;
        }
    };
    let Some(node) = object.downcast::<Node>() else {
        warn!("Audio/Sink {id} downcast to Node failed");
        return;
    };

    let inner_for_info = Arc::clone(inner);
    node.add_listener(NodeEvents {
        info: Some(Box::new(move |info| {
            // `info` events can arrive incrementally — the server
            // may send follow-ups whose `props` only carry the keys
            // that changed. Overwriting our cache with None on every
            // event drops the good HardwareId we derived from the
            // first full event. Only merge when the new read
            // actually resolves something.
            let hw = alsa_props_to_hardware_id(info.props);
            let nick = info.props.get("node.nick").map(str::to_string);
            let dev_id = info
                .props
                .get("device.id")
                .and_then(|v| v.parse::<u32>().ok());
            let route_dev = info
                .props
                .get("card.profile.device")
                .and_then(|v| v.parse::<u32>().ok());

            let mut candidates = inner_for_info
                .candidates
                .lock()
                .expect("candidates lock poisoned");
            if let Some(c) = candidates.get_mut(&id) {
                if hw.is_some() {
                    c.hardware_id = hw;
                }
                if nick.is_some() {
                    c.nick = nick;
                }
                if dev_id.is_some() {
                    c.device_global_id = dev_id;
                }
                if route_dev.is_some() {
                    c.route_device_index = route_dev;
                }
                debug!(
                    "Audio/Sink info: id={id} nick={:?} hardware_id={:?}",
                    c.nick, c.hardware_id
                );
            }
            drop(candidates);

            // Bind the owning Device (if not already bound) so we
            // can use Device::set_param(Route) for the reverse path.
            if let Some(dev_id) = dev_id {
                bind_device_if_needed(&inner_for_info, dev_id);
            }
        })),
        ..Default::default()
    });

    inner
        .candidates
        .lock()
        .expect("candidates lock poisoned")
        .insert(
            id,
            SinkCandidate {
                node,
                hardware_id: None,
                nick: None,
                device_global_id: None,
                route_device_index: None,
            },
        );
}

/// Registry callback: a global disappeared. Remove from the
/// candidate map and, if we were bound to it, drop the binding so
/// the coordinator can rebind to something else on the next pass.
fn handle_global_removed(inner: &Arc<Inner>, id: u32) {
    inner
        .candidates
        .lock()
        .expect("candidates lock poisoned")
        .remove(&id);
    let mut bindings = inner.bindings.lock().expect("bindings lock poisoned");
    if let Some(tv_id) = bindings
        .iter()
        .find(|(_, b)| b.global_id == id)
        .map(|(k, _)| k.clone())
    {
        bindings.remove(&tv_id);
        trace!(tv_id, global_id = id, "sink disappeared; binding dropped");
    }
}

/// Bind a PipeWire Device proxy if we haven't already, and subscribe
/// to its Route params so we learn the route index for each output.
/// Called from the PW thread (inside a Node info callback).
fn bind_device_if_needed(inner: &Arc<Inner>, device_global_id: u32) {
    {
        let devices = inner.devices.lock().expect("devices lock poisoned");
        if devices.contains_key(&device_global_id) {
            return;
        }
    }

    let object = match inner
        .registry
        .bind(device_global_id, types::interface::DEVICE, 3)
    {
        Ok(o) => o,
        Err(e) => {
            warn!(device_global_id, error = %e, "Registry::bind(Device) failed");
            return;
        }
    };
    let Some(device) = object.downcast::<Device>() else {
        warn!(device_global_id, "Device downcast failed");
        return;
    };

    if let Err(e) = device.subscribe_params(&[ParamType::Route]) {
        warn!(device_global_id, error = %e, "Device::subscribe_params(Route) failed");
        return;
    }

    let inner_for_param = Arc::clone(inner);
    let dev_gid = device_global_id;
    device.add_listener(DeviceEvents {
        param: Some(Box::new(
            move |_seq, param_id, _index, _next, pod| {
                if param_id != ParamType::Route {
                    return;
                }
                handle_device_route(&inner_for_param, dev_gid, pod);
            },
        )),
        ..Default::default()
    });

    inner
        .devices
        .lock()
        .expect("devices lock poisoned")
        .insert(
            device_global_id,
            DeviceState {
                device,
                routes: HashMap::new(),
            },
        );
    debug!(device_global_id, "bound Device for route-level volume control");
}

/// Parse a Route param from a Device and store the route index keyed
/// by the route's internal device index. This lets `push_tv_state`
/// look up: Node → `card.profile.device` → route index → set_param.
fn handle_device_route(inner: &Arc<Inner>, device_global_id: u32, pod: &spa::pod::RawPodOwned) {
    use spa::pod::parser::Parser;

    let mut parser = Parser::new(pod.data());
    let mut route_index: Option<u32> = None;
    let mut route_device: Option<u32> = None;

    let _ = parser.pop_object_raw::<u32, _>(|obj, _obj_type, _id| {
        for (key_id, _flags, prop_pod) in obj {
            let Ok(key) = Route::try_from(key_id) else {
                continue;
            };
            match key {
                Route::Index => {
                    if let Ok(v) = <i32 as spa::pod::Pod>::decode(prop_pod.data()) {
                        route_index = Some(v.0 as u32);
                    }
                }
                Route::Device => {
                    if let Ok(v) = <i32 as spa::pod::Pod>::decode(prop_pod.data()) {
                        route_device = Some(v.0 as u32);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    });

    if let (Some(ri), Some(rd)) = (route_index, route_device) {
        let mut devices = inner.devices.lock().expect("devices lock poisoned");
        if let Some(ds) = devices.get_mut(&device_global_id) {
            ds.routes.insert(rd, ri);
            trace!(
                device_global_id,
                route_index = ri,
                route_device = rd,
                "Device route learned"
            );
        }
    }
}

/// Read ALSA props on a PipeWire node and, if it looks like an HDMI
/// PCM with a live downstream display, derive a [`HardwareId`] from
/// the ALSA ELD dump for that connector. Returns `None` for non-ALSA
/// or non-HDMI sinks and for ALSA cards whose ELDs we couldn't read.
///
/// The ELD file name `eld#A.B` uses HDA codec-internal indices (pin
/// NID / codec_dev_id), not the ALSA PCM `device` number that
/// PipeWire exposes, so there's no direct filename derivation. We
/// instead scan every ELD file on the card, pick the ones with
/// `monitor_present = 1`, and match against the sink's `node.nick`
/// (which is the same `monitor_name` ALSA dumps into the ELD file).
fn alsa_props_to_hardware_id(props: &Properties) -> Option<HardwareId> {
    // PipeWire reports either `alsa.card` or the legacy
    // `api.alsa.pcm.card` alias — accept both.
    let card = props
        .get("alsa.card")
        .or_else(|| props.get("api.alsa.pcm.card"))?
        .parse::<u32>()
        .ok()?;

    // `node.nick` is populated from the ELD `monitor_name` by
    // PipeWire. When it's missing we can still fall back to the first
    // live ELD on the card (single-HDMI systems).
    let hint = props
        .get("node.nick")
        .or_else(|| props.get("alsa.name"))
        .unwrap_or("");

    let dir = format!("/proc/asound/card{card}");
    let entries = std::fs::read_dir(&dir).ok()?;

    let mut fallback: Option<HardwareId> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("eld#") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(hw) = parse_eld(&text) else {
            continue;
        };
        if !hint.is_empty() {
            if hw.model.as_deref() == Some(hint) {
                return Some(hw);
            }
        } else if fallback.is_none() {
            fallback = Some(hw);
        }
    }
    fallback
}

/// Parse ALSA's text-form ELD dump into a [`HardwareId`]. Only the
/// monitor_name + manufacture_id fields are needed; serial isn't in
/// ELD anyway (ALSA ships only the audio-relevant EDID subset).
fn parse_eld(text: &str) -> Option<HardwareId> {
    let mut monitor_present = false;
    let mut monitor_name: Option<String> = None;
    let mut manufacture_id: Option<u16> = None;

    for line in text.lines() {
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "monitor_present" => {
                if value == "1" {
                    monitor_present = true;
                }
            }
            "monitor_name" => {
                if !value.is_empty() {
                    monitor_name = Some(value.to_string());
                }
            }
            "manufacture_id" => {
                // Format: "0x6d1e" — little-endian u16 whose bytes
                // are the same 2-byte EDID manufacturer field, just
                // byte-swapped.
                let hex = value.trim_start_matches("0x");
                let word = u16::from_str_radix(hex, 16).ok()?;
                manufacture_id = Some(word.swap_bytes());
            }
            _ => {}
        }
    }

    if !monitor_present {
        return None;
    }
    let manufacturer = manufacture_id.map(pnpid_from_u16);
    if manufacturer.is_none() && monitor_name.is_none() {
        return None;
    }
    Some(HardwareId {
        manufacturer,
        model: monitor_name,
        serial: None,
    })
}

/// Decode a 16-bit EDID manufacturer field into the 3-letter PNPID
/// convention (`GSM` for LG, `SAM` for Samsung, etc.). Each letter
/// is packed into 5 bits as `letter - 'A' + 1`.
fn pnpid_from_u16(word: u16) -> String {
    let l1 = ((word >> 10) & 0x1F) as u8;
    let l2 = ((word >> 5) & 0x1F) as u8;
    let l3 = (word & 0x1F) as u8;
    let to_char = |n: u8| -> char {
        if (1..=26).contains(&n) {
            (b'A' + n - 1) as char
        } else {
            '?'
        }
    };
    [to_char(l1), to_char(l2), to_char(l3)].iter().collect()
}

/// Parse a `Props` pod from the listener callback and emit a
/// [`SinkEvent`] for any change that doesn't match the suppression
/// cache.
fn handle_props_change(
    inner: &Arc<Inner>,
    tv_id: &str,
    event_tx: &mpsc::UnboundedSender<SinkEvent>,
    pod: &spa::pod::RawPodOwned,
) {
    use spa::pod::parser::Parser;

    let mut parser = Parser::new(pod.data());
    let mut volume: Option<u8> = None;
    let mut muted: Option<bool> = None;

    let _ = parser.pop_object_raw::<u32, _>(|obj, _obj_type, _id| {
        for (key_id, _flags, prop_pod) in obj {
            let Ok(prop) = Prop::try_from(key_id) else {
                continue;
            };
            match prop {
                Prop::Mute => {
                    if let Ok(v) = <bool as spa::pod::Pod>::decode(prop_pod.data()) {
                        muted = Some(v.0);
                    }
                }
                Prop::ChannelVolumes => {
                    if let Ok((vs, _)) = <&[f32] as spa::pod::Pod>::decode(prop_pod.data()) {
                        if let Some(v) = average(&vs) {
                            volume = Some(pw_to_u8_volume(v));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    });

    let mut bindings = inner.bindings.lock().expect("bindings lock poisoned");
    let Some(b) = bindings.get_mut(tv_id) else {
        return;
    };
    if let Some(v) = volume {
        if b.last_pushed_volume != Some(v) {
            trace!(tv_id, volume = v, "sink volume change from user");
            b.last_pushed_volume = Some(v);
            let _ = event_tx.send(SinkEvent::VolumeChanged {
                tv_id: tv_id.to_string(),
                volume: v,
            });
        }
    }
    if let Some(m) = muted {
        if b.last_pushed_muted != Some(m) {
            trace!(tv_id, muted = m, "sink mute change from user");
            b.last_pushed_muted = Some(m);
            let _ = event_tx.send(SinkEvent::MuteChanged {
                tv_id: tv_id.to_string(),
                muted: m,
            });
        }
    }
}

fn average(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f32>() / values.len() as f32)
    }
}

/// Build a Props object pod containing `channelVolumes` and `mute`,
/// serialised as raw bytes so it can be embedded inside a Route
/// property via `push_property(Route::Props, ..., raw_pod)`.
fn build_props_pod(channel_volumes: Vec<f32>, muted: bool) -> Result<spa::pod::RawPodOwned> {
    use spa::pod::builder::Builder;

    let mut buf = [0u8; 1024];
    let bytes = Builder::new(&mut buf)
        .push_object(ObjectType::Props, 0u32, |b| {
            b.push_property(Prop::Mute, PropertyFlags::empty(), muted)
                .push_property(
                    Prop::ChannelVolumes,
                    PropertyFlags::empty(),
                    channel_volumes,
                )
        })
        .build()
        .map_err(|e| anyhow!("failed to build Props pod: {e:?}"))?;
    spa::pod::RawPodOwned::wrap(bytes.to_vec())
        .map_err(|e| anyhow!("failed to wrap Props pod: {e:?}"))
}

fn u8_to_pw_volume(v: u8) -> f32 {
    let linear = v.min(100) as f32 / 100.0;
    // PipeWire's channelVolumes is cubic; Plasma displays percent as
    // `cbrt(cv) * 100`. Cube on write so the displayed slider equals
    // our linear percent.
    linear * linear * linear * PW_UNITY_VOLUME
}

fn pw_to_u8_volume(v: f32) -> u8 {
    let unity = (v.max(0.0) / PW_UNITY_VOLUME).min(1.0);
    // Inverse of `u8_to_pw_volume`.
    let linear = unity.cbrt();
    (linear * 100.0).round().clamp(0.0, 100.0) as u8
}

/// Ensure `SPA_PLUGIN_DIR` and `PIPEWIRE_CONFIG_DIR` point at the
/// running PipeWire installation. Called once on volume-sync startup
/// before [`pipewire_native::init`].
fn ensure_pipewire_paths() {
    let Some(root) = autodetect_pipewire_prefix() else {
        return;
    };
    set_if_unset("SPA_PLUGIN_DIR", root.join("lib").join("spa-0.2"));
    set_if_unset("PIPEWIRE_CONFIG_DIR", root.join("share").join("pipewire"));
}

fn set_if_unset(key: &str, path: std::path::PathBuf) {
    if std::env::var_os(key).is_some() || !path.is_dir() {
        return;
    }
    let value = path.to_string_lossy().into_owned();
    // SAFETY: called once, pre-spawn. Race window against unrelated
    // readers is narrow; production packaging replaces this with a
    // wrapper script that sets the env up front.
    unsafe {
        std::env::set_var(key, &value);
    }
    info!(env = key, value = %value, "auto-detected from running PipeWire daemon");
}

fn autodetect_pipewire_prefix() -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let exe_link = entry.path().join("exe");
        let Ok(target) = std::fs::read_link(&exe_link) else {
            continue;
        };
        let file_name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if file_name != "pipewire" {
            continue;
        }
        let prefix = target.parent()?.parent()?.to_path_buf();
        return Some(prefix);
    }
    None
}

// ---------------------------------------------------------------------------
// Coordinator task
// ---------------------------------------------------------------------------

/// Run the volume-sync coordinator until the shutdown signal fires.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    info!("volume_sync task starting");

    let connect_result = tokio::task::spawn_blocking(VolumeSync::connect)
        .await
        .unwrap_or_else(|e| Err(anyhow!("volume_sync connect panicked: {e}")));
    let (volume_sync, event_rx) = match connect_result {
        Ok(pair) => pair,
        Err(e) => {
            warn!("PipeWire not available — volume sync disabled for this session: {e:#}");
            return;
        }
    };

    // Spawn the forward path (sink → TV).
    let forward_handle = tokio::spawn(forward_sink_events(event_rx, daemon.clone()));

    // Initial reconcile.
    reconcile(&volume_sync, &daemon, &state).await;

    let mut watcher = match StateWatcher::new(&daemon, StateWatcherConfig::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "StateWatcher setup failed — exiting volume sync");
            forward_handle.abort();
            return;
        }
    };

    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("Volume sync shutdown signalled");
                break;
            }
            _ = tick.tick() => {
                reconcile(&volume_sync, &daemon, &state).await;
            }
            evt = watcher.next() => {
                match evt {
                    Some(StateEvent::DaemonStateChanged(TvStateChange::PropertiesChanged { tv_id })) => {
                        debug!(tv_id = %tv_id, "StateWatcher: PropertiesChanged");
                        let _ = forward_tv_state(&volume_sync, &daemon, &tv_id).await;
                        reconcile(&volume_sync, &daemon, &state).await;
                    }
                    Some(StateEvent::DaemonStateChanged(_))
                    | Some(StateEvent::FallbackPoll)
                    | Some(StateEvent::DaemonAppeared)
                    | Some(StateEvent::DaemonDisappeared)
                    | Some(StateEvent::SessionProfileChanged) => {
                        reconcile(&volume_sync, &daemon, &state).await;
                    }
                    Some(StateEvent::SessionAppeared | StateEvent::SessionDisappeared) => {}
                    None => {
                        warn!("StateWatcher stream closed; exiting volume sync");
                        break;
                    }
                }
            }
        }
    }

    forward_handle.abort();
    drop(volume_sync);
}

/// Push the TV's current volume / mute into the bound sink.
async fn forward_tv_state(volume_sync: &VolumeSync, daemon: &DaemonClient, tv_id: &str) -> bool {
    let tvs = match daemon.list_tvs().await {
        Ok(v) => v,
        Err(e) => {
            debug!(tv_id, error = %e, "forward_tv_state: list_tvs failed");
            return false;
        }
    };
    let Some(tv) = tvs.into_iter().find(|t| t.id == tv_id) else {
        debug!(tv_id, "forward_tv_state: tv not found in list");
        return false;
    };
    if !tv.connected {
        debug!(tv_id, "forward_tv_state: tv not connected");
        return false;
    }
    debug!(
        tv_id,
        tv.volume, tv.muted, "forward_tv_state: pushing to sink"
    );
    match volume_sync.push_tv_state(tv_id, tv.volume, tv.muted) {
        Ok(()) => true,
        Err(e) => {
            warn!(tv_id, error = %e, "forward_tv_state: push failed");
            false
        }
    }
}

async fn forward_sink_events(
    mut rx: mpsc::UnboundedReceiver<SinkEvent>,
    daemon: Arc<DaemonClient>,
) {
    while let Some(first) = rx.recv().await {
        // Drain-loop coalescing: scroll bursts collapse to the final
        // value; mute is last-write-wins.
        let mut latest_volume: HashMap<String, u8> = HashMap::new();
        let mut latest_muted: HashMap<String, bool> = HashMap::new();
        apply_sink_event(&first, &mut latest_volume, &mut latest_muted);
        while let Ok(next) = rx.try_recv() {
            apply_sink_event(&next, &mut latest_volume, &mut latest_muted);
        }
        for (tv_id, vol) in latest_volume {
            if let Err(e) = daemon.set_volume(&tv_id, vol).await {
                warn!(tv_id, volume = vol, error = %e, "forward volume set failed");
            }
        }
        for (tv_id, m) in latest_muted {
            if let Err(e) = daemon.set_mute(&tv_id, m).await {
                warn!(tv_id, muted = m, error = %e, "forward mute set failed");
            }
        }
    }
    debug!("forward_sink_events drain loop exited");
}

fn apply_sink_event(
    evt: &SinkEvent,
    volumes: &mut HashMap<String, u8>,
    mutes: &mut HashMap<String, bool>,
) {
    match evt {
        SinkEvent::VolumeChanged { tv_id, volume } => {
            volumes.insert(tv_id.clone(), *volume);
        }
        SinkEvent::MuteChanged { tv_id, muted } => {
            mutes.insert(tv_id.clone(), *muted);
        }
    }
}

/// Reconcile the desired bindings against the live TV list + the
/// current set of candidate HDMI sinks.
async fn reconcile(
    volume_sync: &VolumeSync,
    daemon: &DaemonClient,
    state: &Arc<RwLock<SessionState>>,
) {
    let tvs = match daemon.list_tvs().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "list_tvs failed during volume reconcile");
            return;
        }
    };

    let candidates = volume_sync.candidate_identities();

    let desired: Vec<DesiredBinding> = {
        let s = state.read().await;
        tvs.iter()
            .filter_map(|tv| {
                let profile_says_mirror = s
                    .profiles
                    .get(&tv.id)
                    .map(|p| matches!(p.sync_volume, tisvlay_core::SyncVolume::MirrorHdmi))
                    .unwrap_or(false);
                if !(tv.connected && profile_says_mirror) {
                    return None;
                }
                if tv.hardware_id.is_empty() {
                    return None;
                }
                Some(DesiredBinding {
                    tv_id: tv.id.clone(),
                    tv_hw: tv.hardware_id.clone(),
                    volume: tv.volume,
                    muted: tv.muted,
                })
            })
            .collect()
    };

    let desired_ids: std::collections::HashSet<String> =
        desired.iter().map(|d| d.tv_id.clone()).collect();

    // Drop bindings no longer desired.
    for tv_id in volume_sync.bound_tvs() {
        if !desired_ids.contains(&tv_id) {
            volume_sync.unbind_tv(&tv_id);
        }
    }

    debug!(
        "volume reconcile: candidates={} desired={}",
        candidates.len(),
        desired.len()
    );

    // For each desired TV, pick the best candidate sink and bind.
    for d in desired {
        let pick = best_candidate(&candidates, &d.tv_hw);
        match pick {
            Some(global_id) => {
                if let Err(e) = volume_sync.bind_tv(&d.tv_id, global_id, d.volume, d.muted) {
                    warn!(tv_id = %d.tv_id, error = %e, "bind_tv failed");
                }
            }
            None => {
                volume_sync.unbind_tv(&d.tv_id);
                debug!(
                    "no HDMI sink matched for tv_id={} tv_hw={:?} (candidates seen: {:?})",
                    d.tv_id,
                    d.tv_hw,
                    candidates.iter().map(|(_, hw)| hw).collect::<Vec<_>>()
                );
            }
        }
    }
}

struct DesiredBinding {
    tv_id: String,
    tv_hw: HardwareId,
    volume: u8,
    muted: bool,
}

fn best_candidate(candidates: &[(u32, HardwareId)], tv_hw: &HardwareId) -> Option<u32> {
    candidates
        .iter()
        .filter(|(_, hw)| identity_matches(tv_hw, hw))
        .max_by_key(|(_, hw)| match_score(tv_hw, hw))
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u8_to_pw_volume_bounds() {
        assert_eq!(u8_to_pw_volume(0), 0.0);
        assert!(
            (u8_to_pw_volume(50) - 0.125).abs() < 1e-6,
            "50% cubic = 0.125"
        );
        assert_eq!(u8_to_pw_volume(100), 1.0);
        assert_eq!(u8_to_pw_volume(200), 1.0);
    }

    #[test]
    fn pw_to_u8_volume_roundtrip() {
        for v in [0u8, 1, 25, 50, 75, 99, 100] {
            assert_eq!(pw_to_u8_volume(u8_to_pw_volume(v)), v, "round-trip for {v}");
        }
    }

    #[test]
    fn pw_to_u8_volume_clamps() {
        assert_eq!(pw_to_u8_volume(-0.5), 0);
        assert_eq!(pw_to_u8_volume(2.0), 100);
    }

    #[test]
    fn apply_sink_event_keeps_latest() {
        let mut volumes = HashMap::new();
        let mut mutes = HashMap::new();
        apply_sink_event(
            &SinkEvent::VolumeChanged {
                tv_id: "a".into(),
                volume: 10,
            },
            &mut volumes,
            &mut mutes,
        );
        apply_sink_event(
            &SinkEvent::VolumeChanged {
                tv_id: "a".into(),
                volume: 30,
            },
            &mut volumes,
            &mut mutes,
        );
        apply_sink_event(
            &SinkEvent::MuteChanged {
                tv_id: "a".into(),
                muted: true,
            },
            &mut volumes,
            &mut mutes,
        );
        apply_sink_event(
            &SinkEvent::MuteChanged {
                tv_id: "a".into(),
                muted: false,
            },
            &mut volumes,
            &mut mutes,
        );
        assert_eq!(volumes.get("a"), Some(&30));
        assert_eq!(mutes.get("a"), Some(&false));
    }

    #[test]
    fn pnpid_from_u16_lg() {
        // LG's PNPID "GSM":
        //   G=7 -> 0b00111
        //   S=19 -> 0b10011
        //   M=13 -> 0b01101
        // Packed big-endian 16-bit: 0x1E6D.
        assert_eq!(pnpid_from_u16(0x1E6D), "GSM");
    }

    #[test]
    fn parse_eld_extracts_lg_identity() {
        let text = "\
monitor_present		1
eld_valid		1
monitor_name		LG TV SSCR2
connection_type		HDMI
manufacture_id		0x6d1e
product_id		0x81cd
";
        let hw = parse_eld(text).expect("parse");
        assert_eq!(hw.manufacturer.as_deref(), Some("GSM"));
        assert_eq!(hw.model.as_deref(), Some("LG TV SSCR2"));
        assert!(hw.serial.is_none());
    }

    #[test]
    fn parse_eld_rejects_missing_monitor() {
        let text = "monitor_present\t\t0\n";
        assert!(parse_eld(text).is_none());
    }
}
