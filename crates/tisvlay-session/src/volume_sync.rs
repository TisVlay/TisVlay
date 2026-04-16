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
//! Both stages are cubic in amplitude (PipeWire's `channelVolumes`
//! and the TV's volume-to-gain curve both follow `gain = pos^3`).
//! Mirroring the slider 1:1 puts them in series and gives a sextic
//! compound curve — 120 dB/decade, perceptually unusable below the
//! upper third of the slider.
//!
//! Instead we treat the slider position `s ∈ [0, 1]` as the user's
//! desired total attenuation in dB (`60·log₁₀(s)`, the standard
//! 60-dB AES fader stroke) and split that attenuation between the
//! two stages by a configurable factor `α ∈ [0.5, 1.0]`
//! ([`UserProfileConfig::sync_volume_split`], default `0.85`):
//!
//! ```text
//! kde_db = α · 60·log₁₀(s)         → cv = s^(3·α)
//! tv_db  = (1-α) · 60·log₁₀(s)     → tv_value = round(100 · s^(1-α))
//! ```
//!
//! Compound: `cv · (tv/100)³ = s³ · 1 = s³`, identical to a native
//! cubic sink — the slider feels like it would without tisvlay.
//!
//! Side effects of the split:
//! 1. KDE's slider snaps after each user input — the position the
//!    user releases at maps to a slightly higher visual cv (since
//!    `s^α > s` for `α < 1`, `s ∈ (0, 1)`). At `α = 0.85` this is
//!    a few percent at most.
//! 2. The TV's on-screen indicator moves through a smaller stroke
//!    than the slider — necessary to keep the compound from
//!    over-attenuating. `α = 1.0` pins the TV at 100 (KDE-only).
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
    proxy::node::{Node, NodeEvents},
    proxy::registry::{Registry, RegistryEvents},
    thread_loop::ThreadLoop,
    types,
};
use pipewire_native_spa as spa;
use spa::param::ParamType;
use spa::param::props::Prop;
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
    /// Bindings indexed by `tv_id`.
    bindings: Mutex<HashMap<String, Binding>>,
    event_tx: mpsc::UnboundedSender<SinkEvent>,
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
}

struct Binding {
    /// Which candidate we bound against. The proxy lives in
    /// [`SinkCandidate`]; the binding records the dB-split parameter
    /// for this TV's profile, the listener hook so we can detach on
    /// unbind, and two suppression caches:
    ///
    /// 1. `last_written_cv` / `last_written_muted` — what we last
    ///    wrote to KDE's sink Props. The PipeWire param listener
    ///    skips events whose values match (echoes of our own writes).
    /// 2. `last_sent_tv_value` / `last_sent_tv_muted` — what we last
    ///    sent to the TV via the daemon. [`push_tv_state`] skips
    ///    incoming `PropertiesChanged` echoes that match. Without
    ///    this, the round-trip through `u8` quantization drifts the
    ///    cv slightly each time and the forward / reverse paths
    ///    feed each other into a runaway loop toward the slider
    ///    bounds.
    global_id: u32,
    hook_id: pipewire_native::HookId,
    alpha: f32,
    last_written_cv: Option<f32>,
    last_written_muted: Option<bool>,
    last_sent_tv_value: Option<u8>,
    last_sent_tv_muted: Option<bool>,
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
    /// we're already bound to this `(global_id, alpha)` pair we
    /// reuse the binding; if either differs (different sink, or the
    /// resolved profile's `sync_volume_split` changed), the previous
    /// binding is dropped first.
    #[instrument(skip(self))]
    pub fn bind_tv(&self, tv_id: &str, global_id: u32, alpha: f32) -> Result<()> {
        let needs_rebind = {
            let bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
            !matches!(
                bindings.get(tv_id),
                Some(b) if b.global_id == global_id && b.alpha == alpha
            )
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
                    alpha,
                    last_written_cv: None,
                    last_written_muted: None,
                    last_sent_tv_value: None,
                    last_sent_tv_muted: None,
                },
            );

        // No initial push — we don't know whether the TV's current
        // value was set under our split or by the user directly, so
        // back-deriving a slider position would be nonsense (at
        // α=0.85, TV=30 implies a slider near silence). The first
        // listener event seeds `last_written_cv` as baseline (see
        // `handle_props_change`); the first real user input is what
        // establishes the split going forward.

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

    /// Push TV-side state to the bound sink (reverse path).
    ///
    /// `volume` is the TV's reported value (0-100). When the value
    /// matches what we ourselves last sent to the TV we treat it as
    /// an echo from the daemon's `PropertiesChanged` and skip the
    /// write entirely — re-deriving `cv` from `volume` round-trips
    /// through u8 quantization with a tiny drift, and a non-suppressed
    /// echo would feed the listener which would feed another forward,
    /// which would feed another reverse, drifting toward the bounds.
    /// Otherwise we back-derive the slider position from the TV's
    /// share of the split and write the corresponding `cv` to KDE.
    #[instrument(skip(self))]
    pub fn push_tv_state(&self, tv_id: &str, volume: u8, muted: bool) -> Result<()> {
        let (node, alpha) = {
            let bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
            let Some(b) = bindings.get(tv_id) else {
                return Err(anyhow!("no binding for {tv_id}"));
            };
            // Echo of our own send — skip without rewriting the
            // sink. The forward path tracks pending echoes per field
            // (volume vs mute), so a `None` slot means "we didn't
            // send this field, accept whatever the TV reports for
            // it". Mismatch on a `Some` slot means a real TV-remote
            // change; pass through.
            let suppressed = match (b.last_sent_tv_value, b.last_sent_tv_muted) {
                (None, None) => false,
                (Some(v), None) => v == volume,
                (None, Some(m)) => m == muted,
                (Some(v), Some(m)) => v == volume && m == muted,
            };
            if suppressed {
                trace!(tv_id, volume, muted, "TV state matches our last send; suppressing reverse echo");
                drop(bindings);
                let mut bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
                if let Some(b) = bindings.get_mut(tv_id) {
                    b.last_sent_tv_value = None;
                    b.last_sent_tv_muted = None;
                }
                return Ok(());
            }
            let alpha = b.alpha;
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
            (c.node.clone(), alpha)
        };

        let s = tv_value_to_slider(volume, alpha);
        let cv = slider_to_pw_cv(s, alpha);
        // Tokio side: take the loop lock around the write.
        {
            let _g = self.inner.thread_loop.lock();
            write_sink_props(&node, cv, muted)?;
        }

        let mut bindings = self.inner.bindings.lock().expect("bindings lock poisoned");
        if let Some(b) = bindings.get_mut(tv_id) {
            b.last_written_cv = Some(cv);
            b.last_written_muted = Some(muted);
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
                debug!(
                    "Audio/Sink info: id={id} nick={:?} hardware_id={:?}",
                    c.nick, c.hardware_id
                );
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
    let mut new_cv: Option<f32> = None;
    let mut new_muted: Option<bool> = None;

    let _ = parser.pop_object_raw::<u32, _>(|obj, _obj_type, _id| {
        for (key_id, _flags, prop_pod) in obj {
            let Ok(prop) = Prop::try_from(key_id) else {
                continue;
            };
            match prop {
                Prop::Mute => {
                    if let Ok(v) = <bool as spa::pod::Pod>::decode(prop_pod.data()) {
                        new_muted = Some(v.0);
                    }
                }
                Prop::ChannelVolumes => {
                    if let Ok((vs, _)) = <&[f32] as spa::pod::Pod>::decode(prop_pod.data()) {
                        if let Some(avg) = average(&vs) {
                            new_cv = Some(avg);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    });

    // Snapshot the binding state we need (alpha + suppression cache +
    // node) under one lock acquisition. We mutate the cache below
    // after we've decided what to write.
    let (alpha, last_cv, last_muted, node) = {
        let bindings = inner.bindings.lock().expect("bindings lock poisoned");
        let Some(b) = bindings.get(tv_id) else {
            return;
        };
        let candidates = inner.candidates.lock().expect("candidates lock poisoned");
        let node = candidates.get(&b.global_id).map(|c| c.node.clone());
        (b.alpha, b.last_written_cv, b.last_written_muted, node)
    };

    let cv_changed = match (new_cv, last_cv) {
        (Some(cv), Some(prev)) => (cv - prev).abs() > CV_EPS,
        _ => false,
    };
    let mute_changed = match (new_muted, last_muted) {
        (Some(m), Some(prev)) => m != prev,
        _ => false,
    };
    debug!(
        "Props event: tv_id={tv_id} new_cv={new_cv:?} new_muted={new_muted:?} \
         last_cv={last_cv:?} last_muted={last_muted:?} \
         cv_changed={cv_changed} mute_changed={mute_changed}"
    );
    if !cv_changed && !mute_changed {
        // Either an echo of our own write (values match cached
        // `last_written_*`) or the first event after bind, before
        // we've written anything — in that case there's no user
        // input yet, just the sink's current state. Record as the
        // baseline and move on.
        let mut bindings = inner.bindings.lock().expect("bindings lock poisoned");
        if let Some(b) = bindings.get_mut(tv_id) {
            if b.last_written_cv.is_none() {
                b.last_written_cv = new_cv;
            }
            if b.last_written_muted.is_none() {
                b.last_written_muted = new_muted;
            }
        }
        return;
    }

    // Split the forward path by what the user actually changed. A
    // mute toggle must NOT recompute cv — re-applying `cv^α` on every
    // toggle amplifies the cv on each pass and runs the slider away
    // to a rail. Cv changes do override KDE's cv to the rebalanced
    // value (`cv_corrected`) so the slider lands where the dB split
    // says it should.

    if cv_changed {
        let s = pw_cv_to_slider(new_cv.expect("cv_changed implies new_cv is Some"));
        let cv_corrected = slider_to_pw_cv(s, alpha);
        let tv_value = slider_to_tv_value(s, alpha);
        // `write_sink_props` rewrites both cv and mute atomically;
        // pass the current mute state so we don't toggle it as a side
        // effect of a volume change.
        let muted_out = new_muted.or(last_muted).unwrap_or(false);

        if let Some(node) = &node {
            // On the PipeWire thread inside a callback: the before
            // hook already holds the loop mutex, so do NOT relock.
            if let Err(e) = write_sink_props(node, cv_corrected, muted_out) {
                warn!(tv_id, error = %e, "failed to override sink cv after user input");
            }
        }

        let mut bindings = inner.bindings.lock().expect("bindings lock poisoned");
        if let Some(b) = bindings.get_mut(tv_id) {
            b.last_written_cv = Some(cv_corrected);
            b.last_written_muted = Some(muted_out);
            b.last_sent_tv_value = Some(tv_value);
        }
        drop(bindings);

        debug!("forward to TV: tv_id={tv_id} slider={s} tv_value={tv_value} cv_corrected={cv_corrected}");
        let _ = event_tx.send(SinkEvent::VolumeChanged {
            tv_id: tv_id.to_string(),
            volume: tv_value,
        });
    }

    if mute_changed {
        let muted_out = new_muted.expect("mute_changed implies new_muted is Some");
        // KDE already reflects the user's mute toggle correctly — no
        // need to override the sink. Just record the new state and
        // forward to the TV.
        let mut bindings = inner.bindings.lock().expect("bindings lock poisoned");
        if let Some(b) = bindings.get_mut(tv_id) {
            b.last_written_muted = Some(muted_out);
            b.last_sent_tv_muted = Some(muted_out);
        }
        drop(bindings);

        debug!("forward to TV: tv_id={tv_id} muted={muted_out}");
        let _ = event_tx.send(SinkEvent::MuteChanged {
            tv_id: tv_id.to_string(),
            muted: muted_out,
        });
    }
}

/// Float epsilon used for `channelVolumes` echo suppression. KDE
/// scrolls in fixed-precision steps so any genuine change is
/// well above this.
const CV_EPS: f32 = 1e-4;

fn average(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f32>() / values.len() as f32)
    }
}

/// Slider position (`s ∈ [0, 1]`) → KDE `channelVolumes`.
/// Inverts to `pw_cv_to_slider`.
fn slider_to_pw_cv(s: f32, alpha: f32) -> f32 {
    s.clamp(0.0, 1.0).powf(3.0 * alpha) * PW_UNITY_VOLUME
}

/// KDE `channelVolumes` → slider position (the user-visible percent
/// Plasma's cubic taper would display). Note this uses *KDE's* fixed
/// cubic (`p = cbrt(cv)`), not the α-weighted inverse — α only
/// applies to how we rebalance around the user's intent, not to how
/// KDE natively interprets the slider.
fn pw_cv_to_slider(cv: f32) -> f32 {
    (cv.max(0.0) / PW_UNITY_VOLUME).min(1.0).cbrt()
}

/// Slider position → TV volume (0-100 u8).
fn slider_to_tv_value(s: f32, alpha: f32) -> u8 {
    let f_t = s.clamp(0.0, 1.0).powf(1.0 - alpha);
    (f_t * 100.0).round().clamp(0.0, 100.0) as u8
}

/// TV volume → slider position. When `alpha = 1.0` the TV is pinned
/// at 100 in the forward direction, so any reverse-path read with a
/// different value is treated as the user nudging the TV up/down
/// through its own remote — interpreted as a linear slider hint.
fn tv_value_to_slider(v: u8, alpha: f32) -> f32 {
    let f_t = (v.min(100) as f32) / 100.0;
    let one_minus_alpha = 1.0 - alpha;
    if one_minus_alpha < f32::EPSILON {
        return f_t;
    }
    f_t.powf(1.0 / one_minus_alpha)
}

/// Write `channelVolumes` + `mute` to the bound sink.
///
/// **Caller must either be on the PipeWire thread (i.e. inside a
/// callback) or hold `thread_loop.lock()`.** `Node::set_param`
/// requires the loop mutex, but acquiring it from within a callback
/// self-deadlocks because the PW thread's `before` hook already
/// holds it. The call sites that need locking handle it themselves.
fn write_sink_props(node: &Node, cv: f32, muted: bool) -> Result<()> {
    let cv_clamped = cv.clamp(0.0, PW_UNITY_VOLUME);
    let channel_volumes = vec![cv_clamped, cv_clamped];
    node.set_param(
        ParamType::Props,
        ObjectType::Props,
        0,
        Box::new(move |builder| {
            builder
                .push_property(Prop::Mute, PropertyFlags::empty(), muted)
                .push_property(
                    Prop::ChannelVolumes,
                    PropertyFlags::empty(),
                    channel_volumes,
                )
        }),
    )
    .context("Node::set_param(Props)")?;
    Ok(())
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
        Err(_) => return false,
    };
    let Some(tv) = tvs.into_iter().find(|t| t.id == tv_id) else {
        return false;
    };
    if !tv.connected {
        return false;
    }
    volume_sync
        .push_tv_state(tv_id, tv.volume, tv.muted)
        .is_ok()
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
                let profile = s.profiles.get(&tv.id)?;
                if !matches!(profile.sync_volume, tisvlay_core::SyncVolume::MirrorHdmi) {
                    return None;
                }
                if !tv.connected || tv.hardware_id.is_empty() {
                    return None;
                }
                Some(DesiredBinding {
                    tv_id: tv.id.clone(),
                    tv_hw: tv.hardware_id.clone(),
                    alpha: profile.sync_volume_split,
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
                if let Err(e) = volume_sync.bind_tv(&d.tv_id, global_id, d.alpha) {
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
    alpha: f32,
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

    /// Compound gain (dB) from slider position `s` for the given α
    /// split, assuming a cubic TV. Used by the tests below to assert
    /// the "single cubic compound" property.
    fn compound_db(s: f32, alpha: f32) -> f32 {
        let cv = slider_to_pw_cv(s, alpha);
        let tv = slider_to_tv_value(s, alpha) as f32 / 100.0;
        let amp = cv * tv * tv * tv;
        20.0 * amp.log10()
    }

    #[test]
    fn slider_endpoints_round_trip() {
        for alpha in [0.5_f32, 0.85, 1.0] {
            assert_eq!(slider_to_pw_cv(0.0, alpha), 0.0);
            assert!((slider_to_pw_cv(1.0, alpha) - 1.0).abs() < 1e-6);
            assert_eq!(slider_to_tv_value(1.0, alpha), 100);
        }
        // At α < 1 both stages converge to zero. At α = 1 the TV is
        // pinned at 100 even when the slider is at zero — silence
        // comes from KDE's `cv = 0`.
        assert_eq!(slider_to_tv_value(0.0, 0.5), 0);
        assert_eq!(slider_to_tv_value(0.0, 0.85), 0);
        assert_eq!(slider_to_tv_value(0.0, 1.0), 100);
    }

    #[test]
    fn pw_cv_to_slider_is_kde_cubic_inverse() {
        // `pw_cv_to_slider` decodes KDE's native cubic: writing
        // `cv = p^3` and reading it back should round-trip to `p`.
        for p in [0.0_f32, 0.05, 0.25, 0.5, 0.75, 1.0] {
            let cv = p * p * p;
            let p_back = pw_cv_to_slider(cv);
            assert!((p - p_back).abs() < 1e-4, "p={p} cv={cv} back={p_back}");
        }
    }

    #[test]
    fn override_visibly_snaps_slider_by_alpha() {
        // When we override KDE's `cv = p^3` with `cv_target = p^(3α)`,
        // Plasma's slider redraws to `cbrt(cv_target) = p^α`. The
        // snap is small for α close to 1 and larger for smaller α.
        for &(alpha, p, expected_visible) in &[
            (1.0_f32, 0.5, 0.5),
            (0.85, 0.5, 0.5_f32.powf(0.85)),
            (0.5, 0.5, 0.5_f32.sqrt()),
        ] {
            let cv_target = slider_to_pw_cv(p, alpha);
            let visible = pw_cv_to_slider(cv_target);
            assert!(
                (visible - expected_visible).abs() < 1e-4,
                "α={alpha} p={p} visible={visible} expected={expected_visible}"
            );
        }
    }

    #[test]
    fn compound_matches_native_cubic() {
        // The whole point of the dB split: compound gain at slider s
        // matches a single-cubic native sink (60 · log10(s) dB) within
        // the rounding floor introduced by quantising tv_value to u8.
        for alpha in [0.5_f32, 0.7, 0.85, 1.0] {
            for s in [0.1_f32, 0.25, 0.5, 0.75, 1.0] {
                let target = 60.0 * s.log10();
                let actual = compound_db(s, alpha);
                let diff = (actual - target).abs();
                // 1 dB tolerance covers tv u8 quantisation across the
                // tested split values; tighter only at α=1.0 where TV
                // is pinned at exactly 100.
                assert!(
                    diff < 1.0,
                    "α={alpha} s={s} target={target:.2} dB actual={actual:.2} dB"
                );
            }
        }
    }

    #[test]
    fn alpha_one_pins_tv_at_max() {
        for s in [0.0_f32, 0.25, 0.5, 1.0] {
            assert_eq!(slider_to_tv_value(s, 1.0), 100);
        }
    }

    #[test]
    fn tv_value_to_slider_round_trip() {
        let alpha = 0.85;
        // Going TV → slider → TV at integer TV values rounds within
        // 1 unit of u8 quantisation.
        for v in [50_u8, 70, 80, 90, 100] {
            let s = tv_value_to_slider(v, alpha);
            let v_back = slider_to_tv_value(s, alpha);
            let diff = (v as i32 - v_back as i32).abs();
            assert!(diff <= 1, "v={v} s={s} back={v_back}");
        }
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
