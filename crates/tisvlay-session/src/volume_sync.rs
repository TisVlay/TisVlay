//! Volume sync — PipeWire virtual sinks driven by KDE's volume widget.
//!
//! For every managed TV whose effective user profile selects
//! `sync_volume = "virtual_sink"` and whose backend advertises
//! `Capabilities::VOLUME`, the session service creates a PipeWire
//! virtual sink named `tisvlay_<tv_id>`. When the user scrolls KDE's
//! volume widget on that sink, we forward the new `channelVolumes` /
//! `mute` to the TV via `Tv1.Volume`. TV-initiated volume changes
//! (remote control) push back into the sink's `Props` so the desktop
//! slider stays in sync.
//!
//! # Runtime model
//!
//! `pipewire-native` is synchronous — it runs its own C event loop on
//! an internal thread ([`ThreadLoop`]). All PipeWire state lives on
//! that thread. From the tokio side we serialise PipeWire operations
//! behind `thread_loop.lock()` (brief critical sections). Callbacks
//! fire on the PipeWire thread and post into a
//! [`tokio::sync::mpsc::UnboundedSender`] that the coordinator drains.
//!
//! # Feedback-loop suppression
//!
//! When the TV reports a new volume, we push it to the virtual sink
//! via `Node::set_param(Props)`. That same `Props` change triggers the
//! sink's `param` listener, which would otherwise cause us to
//! immediately write it back to the TV. We break the loop by
//! remembering the last value we pushed and discarding listener
//! events whose values match.
//!
//! # Lifecycle
//!
//! Sinks are created/destroyed by the coordinator on every reconcile
//! tick: TV appears / becomes `Connected=true` / profile sets
//! `sync_volume="virtual_sink"` → create. TV disappears / disconnects
//! / profile flips to `"none"` → destroy. Session exit drops the
//! [`VolumeSync`] which destroys every outstanding sink in one pass
//! and quits the [`ThreadLoop`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use pipewire_native::{
    context::Context as PwContext,
    core::Core,
    properties::Properties,
    proxy::node::{Node, NodeEvents},
    thread_loop::ThreadLoop,
    types,
};
use pipewire_native_spa as spa;
use spa::param::ParamType;
use spa::param::props::Prop;
use spa::pod::types::{ObjectType, PropertyFlags};
use tisvlay_core::{DaemonClient, StateEvent, StateWatcher, StateWatcherConfig, TvStateChange};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, instrument, trace, warn};

use crate::SessionState;

/// `channelVolumes` value that corresponds to full (0 dB) gain.
/// PipeWire uses a cubic scale in `channelVolumes` where `1.0` is
/// unity; we treat the host-side `u8` Volume (0-100) as a linear
/// percentage of that unity.
const PW_UNITY_VOLUME: f32 = 1.0;

/// Events emitted from the PipeWire thread into the coordinator —
/// user-initiated sink changes that need to reach the TV.
#[derive(Debug, Clone)]
pub enum SinkEvent {
    /// New volume (0-100) for the given TV.
    VolumeChanged { tv_id: String, volume: u8 },
    /// New mute state for the given TV.
    MuteChanged { tv_id: String, muted: bool },
}

/// Owns the PipeWire connection and the map of virtual sinks.
///
/// Cloneable — internal state is refcounted. Drop the last clone to
/// tear down every outstanding sink and quit the [`ThreadLoop`].
#[derive(Clone)]
pub struct VolumeSync {
    inner: Arc<Inner>,
}

struct Inner {
    thread_loop: ThreadLoop,
    core: Core,
    sinks: Mutex<HashMap<String, SinkEntry>>,
    event_tx: mpsc::UnboundedSender<SinkEvent>,
}

struct SinkEntry {
    /// Typed node proxy. `Node` is refcounted; holding it here keeps
    /// the server-side object alive until we explicitly destroy it.
    node: Node,
    /// Suppression cache: the last value we wrote via [`push_tv_state`].
    /// Listener events whose new value matches these are discarded so
    /// the reverse path doesn't echo back to the TV.
    last_pushed_volume: Option<u8>,
    last_pushed_muted: Option<bool>,
}

impl VolumeSync {
    /// Initialise PipeWire and start the thread loop.
    ///
    /// Returns the handle and a receiver of [`SinkEvent`]s. The
    /// coordinator must drain the receiver or the PipeWire callback
    /// thread will back up on the unbounded channel (memory growth
    /// only — no deadlock).
    pub fn connect() -> Result<(Self, mpsc::UnboundedReceiver<SinkEvent>)> {
        // `pipewire-native` looks for SPA plugins and config files
        // under paths baked in at build time (typically
        // `/usr/lib/spa-0.2` and `/usr/share/pipewire/`). On NixOS
        // those live inside the Nix store under a versioned
        // derivation path, so the defaults miss. Auto-detect by
        // walking `/proc` for the running PipeWire daemon and
        // deriving the paths relative to its exe.
        ensure_pipewire_paths();

        // `init()` is idempotent via a `OnceLock`.
        pipewire_native::init();

        let thread_loop = ThreadLoop::new(&Properties::new())
            .ok_or_else(|| anyhow!("pipewire-native failed to create ThreadLoop"))?;
        let pw_context = PwContext::new(thread_loop.main_loop(), Properties::new())
            .context("pipewire-native Context::new")?;

        // Start the loop thread before connecting so socket I/O has
        // a runner attached.
        thread_loop.run();

        let core = {
            let _g = thread_loop.lock();
            pw_context
                .connect(None)
                .context("pipewire-native Core::connect")?
        };

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let this = Self {
            inner: Arc::new(Inner {
                thread_loop,
                core,
                sinks: Mutex::new(HashMap::new()),
                event_tx,
            }),
        };
        info!("PipeWire connection established for volume sync");
        Ok((this, event_rx))
    }

    /// Create a virtual sink for `tv_id` if we don't already have one.
    #[instrument(skip(self))]
    pub fn register_tv(
        &self,
        tv_id: &str,
        tv_name: &str,
        initial_volume: u8,
        initial_muted: bool,
    ) -> Result<()> {
        {
            let sinks = self.inner.sinks.lock().expect("sinks lock poisoned");
            if sinks.contains_key(tv_id) {
                return Ok(());
            }
        }

        let node_name = format!("tisvlay_{tv_id}");
        let description = if tv_name.is_empty() {
            tv_id.to_string()
        } else {
            tv_name.to_string()
        };

        let mut props = Properties::new();
        props.set("factory.name", "support.null-audio-sink".to_string());
        props.set("node.name", node_name.clone());
        props.set("node.description", description);
        props.set("media.class", "Audio/Sink".to_string());
        props.set("audio.position", "FL,FR".to_string());

        let (node, _hook_id) = {
            let _g = self.inner.thread_loop.lock();
            let object = self
                .inner
                .core
                .create_object("adapter", types::interface::NODE, 3, &props)
                .context("Core::create_object failed")?;
            let node = object
                .downcast::<Node>()
                .ok_or_else(|| anyhow!("created object is not a Node proxy"))?;

            node.subscribe_params(&[ParamType::Props])
                .context("Node::subscribe_params(Props) failed")?;

            let event_tx = self.inner.event_tx.clone();
            let tv_id_for_cb = tv_id.to_string();
            let inner_for_cb = Arc::clone(&self.inner);
            let hook_id = node.add_listener(NodeEvents {
                info: None,
                param: Some(Box::new(
                    move |_seq, param_id, _index, _next, pod: &spa::pod::RawPodOwned| {
                        if param_id != ParamType::Props {
                            return;
                        }
                        handle_props_change(&inner_for_cb, &tv_id_for_cb, &event_tx, pod);
                    },
                )),
            });
            (node, hook_id)
        };

        self.inner
            .sinks
            .lock()
            .expect("sinks lock poisoned")
            .insert(
                tv_id.to_string(),
                SinkEntry {
                    node,
                    last_pushed_volume: Some(initial_volume),
                    last_pushed_muted: Some(initial_muted),
                },
            );

        // Seed the sink with the TV's current state so the desktop
        // slider starts where the user left the TV. Errors here
        // don't block registration — worst case the slider shows 0
        // until the next reconcile.
        if let Err(e) = self.push_tv_state(tv_id, initial_volume, initial_muted) {
            warn!(tv_id, error = %e, "failed to seed initial volume");
        }

        info!(tv_id, node_name = %node_name, "virtual sink created");
        Ok(())
    }

    /// Destroy the virtual sink for `tv_id` if present. Idempotent.
    #[instrument(skip(self))]
    pub fn unregister_tv(&self, tv_id: &str) {
        let entry = {
            let mut sinks = self.inner.sinks.lock().expect("sinks lock poisoned");
            sinks.remove(tv_id)
        };
        if let Some(entry) = entry {
            let _g = self.inner.thread_loop.lock();
            if let Err(e) = self.inner.core.destroy(&entry.node) {
                warn!(tv_id, error = %e, "core.destroy failed for virtual sink");
            }
            info!(tv_id, "virtual sink destroyed");
        }
    }

    /// Push TV-side state to the virtual sink (reverse path).
    ///
    /// Updates `Props.channelVolumes` and `Props.mute`. Records the
    /// values for feedback-loop suppression.
    #[instrument(skip(self))]
    pub fn push_tv_state(&self, tv_id: &str, volume: u8, muted: bool) -> Result<()> {
        let node = {
            let sinks = self.inner.sinks.lock().expect("sinks lock poisoned");
            let Some(entry) = sinks.get(tv_id) else {
                return Err(anyhow!("no virtual sink registered for {tv_id}"));
            };
            entry.node.clone()
        };

        let volume_pw = u8_to_pw_volume(volume);
        let channel_volumes = vec![volume_pw, volume_pw];

        {
            let _g = self.inner.thread_loop.lock();
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
        }

        let mut sinks = self.inner.sinks.lock().expect("sinks lock poisoned");
        if let Some(entry) = sinks.get_mut(tv_id) {
            entry.last_pushed_volume = Some(volume);
            entry.last_pushed_muted = Some(muted);
        }
        Ok(())
    }

    /// Currently-registered TV ids. For tests and status readouts.
    pub fn registered(&self) -> Vec<String> {
        self.inner
            .sinks
            .lock()
            .expect("sinks lock poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for VolumeSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VolumeSync")
            .field("registered", &self.registered())
            .finish_non_exhaustive()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let ids: Vec<String> = self
            .sinks
            .lock()
            .expect("sinks lock poisoned")
            .keys()
            .cloned()
            .collect();
        if !ids.is_empty() {
            let mut sinks = self.sinks.lock().expect("sinks lock poisoned");
            let _g = self.thread_loop.lock();
            for id in ids {
                if let Some(entry) = sinks.remove(&id) {
                    let _ = self.core.destroy(&entry.node);
                }
            }
        }
        self.thread_loop.quit();
        info!("PipeWire volume sync shut down");
    }
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

    let mut sinks = inner.sinks.lock().expect("sinks lock poisoned");
    let Some(entry) = sinks.get_mut(tv_id) else {
        return;
    };
    if let Some(v) = volume {
        if entry.last_pushed_volume != Some(v) {
            trace!(tv_id, volume = v, "sink volume change from user");
            entry.last_pushed_volume = Some(v);
            let _ = event_tx.send(SinkEvent::VolumeChanged {
                tv_id: tv_id.to_string(),
                volume: v,
            });
        }
    }
    if let Some(m) = muted {
        if entry.last_pushed_muted != Some(m) {
            trace!(tv_id, muted = m, "sink mute change from user");
            entry.last_pushed_muted = Some(m);
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
    // SAFETY: called once, before any tokio tasks other than our own
    // coordinator are reading env vars. The volume-sync coordinator
    // is the only consumer of these PipeWire variables in this
    // process; the brief race window against unrelated readers
    // (e.g. tokio's signal handler thread) is acceptable for a
    // dev-convenience auto-detect that production packaging will
    // replace with a wrapper script.
    unsafe {
        std::env::set_var(key, &value);
    }
    info!(env = key, value = %value, "auto-detected from running PipeWire daemon");
}

/// Walk `/proc` for a running `pipewire` binary and return its
/// installation prefix (the directory holding `bin/pipewire`).
/// Returns `None` if no running PipeWire is found.
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
        // Expect `<root>/bin/pipewire`.
        let prefix = target.parent()?.parent()?.to_path_buf();
        return Some(prefix);
    }
    None
}

fn u8_to_pw_volume(v: u8) -> f32 {
    (v.min(100) as f32 / 100.0) * PW_UNITY_VOLUME
}

fn pw_to_u8_volume(v: f32) -> u8 {
    let scaled = (v.max(0.0) / PW_UNITY_VOLUME) * 100.0;
    scaled.round().clamp(0.0, 100.0) as u8
}

// ---------------------------------------------------------------------------
// Coordinator task
// ---------------------------------------------------------------------------

/// Run the volume-sync coordinator until the shutdown signal fires.
///
/// Owns the [`VolumeSync`] handle and reconciles virtual-sink
/// registration against the daemon's live TV state on a 10 s tick
/// plus whenever the [`StateWatcher`] reports a change.
pub async fn run(
    daemon: Arc<DaemonClient>,
    state: Arc<RwLock<SessionState>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    info!("volume_sync task starting");

    // `VolumeSync::connect` is synchronous and spawns PipeWire's own
    // thread loop, so run it on the blocking pool to avoid holding
    // the tokio worker during the connect handshake.
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
            warn!(error = %e, "StateWatcher setup failed; running with periodic reconcile only");
            polling_loop(&volume_sync, &daemon, &state, &mut shutdown).await;
            forward_handle.abort();
            drop(volume_sync);
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
                    Some(StateEvent::DaemonStateChanged(change)) => {
                        if let TvStateChange::PropertiesChanged { tv_id } = &change {
                            let _ = forward_tv_state(&volume_sync, &daemon, tv_id).await;
                        }
                        reconcile(&volume_sync, &daemon, &state).await;
                    }
                    Some(StateEvent::FallbackPoll) => {
                        reconcile(&volume_sync, &daemon, &state).await;
                    }
                    Some(StateEvent::DaemonAppeared | StateEvent::DaemonDisappeared) => {
                        reconcile(&volume_sync, &daemon, &state).await;
                    }
                    Some(StateEvent::SessionProfileChanged) => {
                        reconcile(&volume_sync, &daemon, &state).await;
                    }
                    Some(StateEvent::SessionAppeared | StateEvent::SessionDisappeared) => {
                        // Session service lifecycle is not relevant
                        // to volume sync — our state is local.
                    }
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

async fn polling_loop(
    volume_sync: &VolumeSync,
    daemon: &DaemonClient,
    state: &Arc<RwLock<SessionState>>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tick.tick() => {
                reconcile(volume_sync, daemon, state).await;
            }
        }
    }
}

/// Push the TV's current volume / mute into the virtual sink.
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
        // Drain-loop coalescing: for scroll bursts only the final
        // value matters; mute changes are last-write-wins.
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

/// Reconcile the desired sink set against the live TV list.
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

    let desired: HashMap<String, DesiredSink> = {
        let s = state.read().await;
        tvs.iter()
            .map(|tv| {
                let profile_says_virtual = s
                    .profiles
                    .get(&tv.id)
                    .map(|p| matches!(p.sync_volume, tisvlay_core::SyncVolume::VirtualSink))
                    .unwrap_or(false);
                let want = tv.connected && profile_says_virtual;
                (
                    tv.id.clone(),
                    DesiredSink {
                        want,
                        name: tv.name.clone(),
                        volume: tv.volume,
                        muted: tv.muted,
                    },
                )
            })
            .collect()
    };

    let currently: Vec<String> = volume_sync.registered();

    for (tv_id, desired) in &desired {
        let registered = currently.contains(tv_id);
        match (desired.want, registered) {
            (true, false) => {
                if let Err(e) =
                    volume_sync.register_tv(tv_id, &desired.name, desired.volume, desired.muted)
                {
                    warn!(tv_id, error = %e, "register_tv failed");
                }
            }
            (false, true) => {
                volume_sync.unregister_tv(tv_id);
            }
            _ => {}
        }
    }

    // Drop sinks whose TVs have disappeared from the daemon's list.
    for tv_id in &currently {
        if !desired.contains_key(tv_id) {
            volume_sync.unregister_tv(tv_id);
        }
    }
}

struct DesiredSink {
    want: bool,
    name: String,
    volume: u8,
    muted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u8_to_pw_volume_bounds() {
        assert_eq!(u8_to_pw_volume(0), 0.0);
        assert_eq!(u8_to_pw_volume(50), 0.5);
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
}
