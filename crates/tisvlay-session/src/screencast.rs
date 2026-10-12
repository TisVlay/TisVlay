//! PipeWire screencast detector: the `Screencast` remote source.
//!
//! Any KWin-serviced screen capture appears in the PipeWire graph as a
//! node with `media.class = "Stream/Output/Video"` and a `media.name`
//! starting `kwin-screencast-` (krdp in both modes, RustDesk, plus
//! plain captures like OBS or a browser tab-share — the signal carries no
//! app attribution, which is why the unattributed `Screencast` kind arms
//! only when the resolved profile opts in). The detector watches the
//! graph and reports one logical `Screencast` source that is ON while any
//! matching node is present (regardless of the node's run state — capture
//! setup precedes injected input, so arming on mere appearance closes the
//! race) and OFF when the last one goes away. The current node state
//! rides in the source `detail` for observability only.
//!
//! The PipeWire client is `!Send` and drives its own event loop, so the
//! glue lives on a dedicated OS thread; a `pipewire_native::ThreadLoop`
//! owns the connection and pumps registry/node events into the pure
//! [`Tracker`], whose emitted [`RemoteReport`] deltas cross into the
//! session core over the detector-report channel. Everything with any
//! logic is a pure function here; the thread body is the thin,
//! CI-untestable strip.
//!
//! Resilience: PipeWire may be absent (headless CI, tests) or restart.
//! The connect loop retries with backoff and stays quiet after the first
//! warning, and a lost connection tears the source down and reconnects —
//! it never blocks or kills the session service.

#![expect(
    unsafe_code,
    reason = "env::set_var points PipeWire at the host's SPA plugins, under an unsafe fn whose caller guarantees the process is still single-threaded"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tisvlay_protocol::payload::RemoteSourceKind;
use tracing::debug;

use crate::detector::Backoff;
use crate::{RemoteClient, RemoteReport};

/// PipeWire property keys and the values the predicate matches. Plain
/// strings (the crate exposes no typed constants for these), named once.
const MEDIA_CLASS: &str = "media.class";
const MEDIA_NAME: &str = "media.name";
const SCREENCAST_MEDIA_CLASS: &str = "Stream/Output/Video";
/// KWin names each capture stream `kwin-screencast-` plus the output,
/// window or region it captures, and sets no other property; PipeWire
/// copies the stream name into `media.name`. The prefix has held since
/// KWin 5.20. `node.name` won't do: it defaults to the process binary,
/// which packaging renames (`.kwin_wayland-wrapped` under NixOS).
const KWIN_STREAM_PREFIX: &str = "kwin-screencast-";

/// The properties the predicate needs, lifted out of a PipeWire
/// `Properties` bag into an owned struct so the predicate and the tracker
/// are pure and unit-testable without a live graph. `state` is the node's
/// run state as a stable label (`None` until a node-info event arrives),
/// carried for the source `detail` only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NodeProps {
    pub media_class: Option<String>,
    pub media_name: Option<String>,
    pub state: Option<String>,
}

/// The predicate: a KWin-serviced video capture node.
pub(crate) fn is_screencast(props: &NodeProps) -> bool {
    props.media_class.as_deref() == Some(SCREENCAST_MEDIA_CLASS)
        && props.media_name.as_deref().is_some_and(is_kwin_stream_name)
}

fn is_kwin_stream_name(name: &str) -> bool {
    name.starts_with(KWIN_STREAM_PREFIX)
}

/// Tracks the matching nodes present in the graph and turns node-info /
/// removal edges into `Screencast` source reports. Pure: the FFI glue
/// feeds it each bound node's info as `(id, props)` and `id` on removal;
/// it returns the reports to hand to the core.
///
/// Exactly one logical `Screencast` source exists — ON while any matching
/// node is present, its `detail` summarizing the live node states. A
/// state change replaces the detail without ever letting the source lapse
/// (the new detail is reported active before the old is cleared), so the
/// aggregate connect/disconnect edge never spuriously flaps on a mere
/// state transition.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    /// Matching node id → its last-known state label (empty before the
    /// first node-info event).
    nodes: BTreeMap<u32, String>,
    /// The `detail` of the currently-published source, if any.
    current: Option<String>,
}

impl Tracker {
    /// A bound node's info arrived. The first event carries its full
    /// properties (`media.name` among them, which the registry's global
    /// announcement leaves out), later ones its run state. Starts
    /// tracking a node whose properties match and refreshes the detail of
    /// one already tracked; returns any resulting source reports.
    pub(crate) fn on_info(&mut self, id: u32, props: &NodeProps) -> Vec<RemoteReport> {
        if self.nodes.contains_key(&id) {
            return match props.state.as_deref() {
                Some(state) => self.on_state(id, state),
                None => Vec::new(),
            };
        }
        self.on_global(id, props)
    }

    /// A node's full properties are known. Records it if it matches the
    /// predicate; returns any resulting source reports.
    pub(crate) fn on_global(&mut self, id: u32, props: &NodeProps) -> Vec<RemoteReport> {
        if !is_screencast(props) {
            return Vec::new();
        }
        self.nodes
            .entry(id)
            .or_insert_with(|| props.state.clone().unwrap_or_default());
        self.reconcile()
    }

    /// A node-info event updated a node's run state. No-op for nodes we
    /// aren't tracking; otherwise refreshes the detail.
    pub(crate) fn on_state(&mut self, id: u32, state: &str) -> Vec<RemoteReport> {
        let Some(slot) = self.nodes.get_mut(&id) else {
            return Vec::new();
        };
        if slot == state {
            return Vec::new();
        }
        *slot = state.to_string();
        self.reconcile()
    }

    /// A global was removed. Drops it if tracked; returns any reports.
    pub(crate) fn on_removed(&mut self, id: u32) -> Vec<RemoteReport> {
        if self.nodes.remove(&id).is_none() {
            return Vec::new();
        }
        self.reconcile()
    }

    /// Tear the source down entirely (a session ending / a PipeWire
    /// crash): forget every node and return the report that clears the
    /// currently-published source, keyed on its real detail, so nothing
    /// lingers armed across a reconnect.
    pub(crate) fn clear(&mut self) -> Vec<RemoteReport> {
        self.nodes.clear();
        self.reconcile()
    }

    /// Bring the published source in line with the tracked nodes.
    /// Add-new-before-clear-old ordering keeps the source continuously
    /// present across a detail change.
    fn reconcile(&mut self) -> Vec<RemoteReport> {
        let next = self.detail();
        if next == self.current {
            return Vec::new();
        }
        let mut reports = Vec::new();
        if let Some(detail) = &next {
            reports.push(report(detail.clone(), true));
        }
        if let Some(previous) = self.current.take() {
            reports.push(report(previous, false));
        }
        self.current = next;
        reports
    }

    /// The source detail for the current node set: `None` when empty,
    /// else a stable summary of the nodes and their states.
    fn detail(&self) -> Option<String> {
        if self.nodes.is_empty() {
            return None;
        }
        Some(summarize(self.nodes.values().map(String::as_str)))
    }
}

/// Summarize the live node states into a stable `detail` string, e.g.
/// `1 node (running)` or `2 nodes (idle, running)`. Deterministic
/// (states sorted + deduped) so an unchanged node set yields an unchanged
/// detail and the source publish is idempotent.
fn summarize<'a>(states: impl Iterator<Item = &'a str>) -> String {
    let mut labels: Vec<&str> = states
        .map(|s| if s.is_empty() { "unknown" } else { s })
        .collect();
    let count = labels.len();
    labels.sort_unstable();
    labels.dedup();
    let noun = if count == 1 { "node" } else { "nodes" };
    format!("{count} {noun} ({})", labels.join(", "))
}

/// A `Screencast` source report with the given detail and active state.
fn report(detail: String, active: bool) -> RemoteReport {
    RemoteReport {
        kind: RemoteSourceKind::Screencast,
        detail,
        active,
        provisional: false,
    }
}

/// Spawn the detector on its own OS thread. Returns immediately; the
/// thread supervises its own PipeWire connection and never blocks the
/// caller. Reports flow through `client` into the session core.
pub fn spawn(client: RemoteClient) {
    std::thread::Builder::new()
        .name("tisvlay-screencast".into())
        .spawn(move || run(client))
        .expect("spawn screencast detector thread");
}

/// Supervised connect loop: (re)connect to PipeWire, run a session until
/// it ends (server gone / error), back off, retry. The session clears its
/// own source on teardown, so a crash mid-capture disarms rather than
/// leaving a stale source live. Quiet after the first warning so a
/// headless box doesn't spam the log. Returns only if the core loop is
/// gone (nothing left to report to).
fn run(client: RemoteClient) {
    ffi::init();
    let mut backoff = Backoff::default();
    loop {
        match ffi::run_session(&client) {
            Ok(()) => {
                // Clean session end (server disconnect). Reset backoff —
                // a returning server reconnects promptly.
                debug!("screencast: pipewire session ended; reconnecting");
                backoff.reset();
            }
            Err(FfiError::CoreGone) => return,
            Err(FfiError::Connect(e)) => backoff.note_failure("screencast: pipewire", e),
        }
        std::thread::sleep(backoff.next_delay());
    }
}

/// Why a PipeWire session ended.
enum FfiError {
    /// The connection could not be established or faulted.
    Connect(std::io::Error),
    /// The session core loop is gone — stop supervising.
    CoreGone,
}

impl From<std::io::Error> for FfiError {
    fn from(e: std::io::Error) -> Self {
        Self::Connect(e)
    }
}

pub use ffi::point_at_host_spa_plugins;

/// No SPA support plugin in any directory PipeWire's loader would search,
/// so the screencast detector can't start.
#[derive(Debug)]
pub struct SpaPluginsMissing {
    /// The directories checked, in search order.
    pub searched: Vec<PathBuf>,
}

impl std::fmt::Display for SpaPluginsMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let searched: Vec<String> = self
            .searched
            .iter()
            .map(|dir| dir.display().to_string())
            .collect();
        write!(
            f,
            "no SPA support plugin in {}; set SPA_PLUGIN_DIR to PipeWire's spa-0.2 directory",
            searched.join(", ")
        )
    }
}

/// What to do about `SPA_PLUGIN_DIR` before the PipeWire client starts.
#[derive(Debug, PartialEq, Eq)]
enum SpaPlan {
    /// The operator's `SPA_PLUGIN_DIR` holds the support plugin: leave it.
    KeepPinned,
    /// Point the loader at these directories, in search order.
    Point(Vec<PathBuf>),
}

/// Decide where PipeWire's loader should look for its support plugin:
/// an operator's `pinned` list is kept if any entry has it, and otherwise
/// the `candidates` that have it are used. `has_plugin` says whether a
/// directory holds the plugin, the one file the loader can't start
/// without. Neither having it means the detector must stay off.
fn plan_spa_dirs(
    pinned: Option<Vec<PathBuf>>,
    candidates: Vec<PathBuf>,
    has_plugin: impl Fn(&Path) -> bool,
) -> Result<SpaPlan, SpaPluginsMissing> {
    if let Some(pinned) = pinned {
        return if pinned.iter().any(|dir| has_plugin(dir)) {
            Ok(SpaPlan::KeepPinned)
        } else {
            Err(SpaPluginsMissing { searched: pinned })
        };
    }
    let usable: Vec<PathBuf> = candidates
        .iter()
        .filter(|dir| has_plugin(dir))
        .cloned()
        .collect();
    if usable.is_empty() {
        Err(SpaPluginsMissing {
            searched: candidates,
        })
    } else {
        Ok(SpaPlan::Point(usable))
    }
}

/// Where SPA plugins may live, in search order and each once: `baked`
/// (the plugin dir pkg-config reported when this crate was built, a Nix
/// store path for a Nix build); the Debian/Ubuntu multiarch dirs
/// `/usr/lib/<triple>/spa-0.2` among `usr_lib` (the entries of
/// `/usr/lib`) for this binary's `arch` only, since another
/// architecture's plugins would pass the file check and then fail to
/// load; then `/usr/lib/spa-0.2` (Arch and others) and
/// `/usr/lib64/spa-0.2` (Fedora/RHEL/openSUSE).
fn spa_candidates(
    baked: Option<&str>,
    usr_lib: impl IntoIterator<Item = PathBuf>,
    arch: &str,
) -> Vec<PathBuf> {
    let triple_prefix = format!("{arch}-");
    let mut multiarch: Vec<PathBuf> = usr_lib
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&triple_prefix))
        })
        .map(|entry| entry.join("spa-0.2"))
        .collect();
    multiarch.sort();
    let fixed = ["/usr/lib/spa-0.2", "/usr/lib64/spa-0.2"].map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in baked
        .map(PathBuf::from)
        .into_iter()
        .chain(multiarch)
        .chain(fixed)
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The FFI-facing strip: the only code that touches `pipewire-native`.
/// Deliberately thin — it wires registry/node events to the pure
/// [`Tracker`] and forwards its reports. Not exercised by CI (needs a
/// live PipeWire server); verified manually.
mod ffi {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use pipewire_native::properties::Properties;
    use pipewire_native::proxy::node::{NodeEvents, NodeState};
    use pipewire_native::proxy::registry::RegistryEvents;
    use pipewire_native::proxy::{HasProxy, ProxyEvents};
    use pipewire_native::{context::Context, some_closure, thread_loop::ThreadLoop, types};

    use std::path::{Path, PathBuf};

    use super::{
        FfiError, NodeProps, SpaPlan, SpaPluginsMissing, Tracker, plan_spa_dirs, spa_candidates,
    };
    use crate::RemoteClient;

    /// One-time global init of the PipeWire support libraries.
    pub(super) fn init() {
        pipewire_native::init();
    }

    /// Point PipeWire's plugin loader at SPA plugins that are really
    /// there, or report that there are none. `pipewire-native` loads its
    /// support plugin (`support/libspa-support.so`) from `SPA_PLUGIN_DIR`,
    /// else from a directory baked in at its build, and panics if it isn't
    /// there. A packaged build rarely matches the host: our `.deb` bakes
    /// pkg-config's fallback `/usr/lib64/spa-0.2` while Debian/Ubuntu ship
    /// the multiarch `/usr/lib/<triple>/spa-0.2`, and a build that found
    /// no pkg-config entry bakes a path a NixOS host doesn't have. So the
    /// candidates (the operator's `SPA_PLUGIN_DIR`, else our own
    /// build-time pkg-config answer and every FHS layout) are checked for
    /// the support plugin, and `SPA_PLUGIN_DIR` is set to the ones that
    /// have it. With none, the detector must stay off.
    ///
    /// # Safety
    ///
    /// It calls [`std::env::set_var`], so no other thread may exist yet: call
    /// it from `main` before the async runtime starts.
    pub unsafe fn point_at_host_spa_plugins() -> Result<(), SpaPluginsMissing> {
        let support_lib =
            std::env::var("SPA_SUPPORT_LIB").unwrap_or_else(|_| "support/libspa-support".into());
        let pinned = std::env::var_os("SPA_PLUGIN_DIR")
            .map(|pinned| std::env::split_paths(&pinned).collect::<Vec<_>>());
        let usr_lib: Vec<PathBuf> = std::fs::read_dir("/usr/lib")
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        let candidates = spa_candidates(
            option_env!("TISVLAY_SPA_PLUGINDIR"),
            usr_lib,
            std::env::consts::ARCH,
        );
        let has_plugin = |dir: &Path| dir.join(format!("{support_lib}.so")).is_file();
        match plan_spa_dirs(pinned, candidates, has_plugin)? {
            SpaPlan::KeepPinned => Ok(()),
            SpaPlan::Point(dirs) => {
                let joined = std::env::join_paths(&dirs)
                    .expect("plugin dirs from the filesystem and pkg-config hold no ':'");
                // SAFETY: by this fn's contract no other thread exists yet,
                // so nothing can read or write the environment concurrently.
                unsafe { std::env::set_var("SPA_PLUGIN_DIR", joined) };
                Ok(())
            }
        }
    }

    /// Shared flags the callbacks flip: `done` on a broken pipe (server
    /// gone), `core_gone` when the session core loop has dropped the
    /// report channel (stop supervising entirely).
    #[derive(Clone, Default)]
    struct Flags {
        done: Arc<AtomicBool>,
        core_gone: Arc<AtomicBool>,
    }

    /// The node state as a stable snake_case label for the source detail.
    fn state_label(state: NodeState) -> &'static str {
        match state {
            NodeState::Error => "error",
            NodeState::Creating => "creating",
            NodeState::Suspended => "suspended",
            NodeState::Idle => "idle",
            NodeState::Running => "running",
        }
    }

    /// Lift the predicate-relevant properties out of a PipeWire bag.
    fn node_props(props: &Properties) -> NodeProps {
        NodeProps {
            media_class: props.get(super::MEDIA_CLASS).map(str::to_string),
            media_name: props.get(super::MEDIA_NAME).map(str::to_string),
            state: None,
        }
    }

    /// Connect to PipeWire, enumerate + watch the graph, and pump the
    /// tracker until the connection ends. On teardown it clears its own
    /// source so nothing lingers armed across a reconnect. `Ok(())` = a
    /// clean disconnect; `Err(Connect)` = the connection could not be
    /// established or faulted; `Err(CoreGone)` = the session core is gone.
    ///
    /// The enumeration (registry `global` for already-present nodes)
    /// provides startup hydration for free.
    pub(super) fn run_session(client: &RemoteClient) -> Result<(), FfiError> {
        let tracker = Arc::new(Mutex::new(Tracker::default()));
        let flags = Flags::default();

        let props = Properties::new();
        let main_loop =
            ThreadLoop::new(&props).ok_or_else(|| std::io::Error::other("thread loop"))?;
        // Skip PipeWire's client.conf: pipewire-native parses it (through
        // the external spa-json-dump tool, which a service unit's PATH may
        // lack) and never reads it, and the detector needs no client
        // config. PIPEWIRE_CONFIG_NAME still overrides this.
        let mut context_props = Properties::new();
        context_props.set(pipewire_native::keys::CONFIG_NAME, "null".into());
        let context = Context::new(main_loop.main_loop(), context_props)?;
        // The Core keeps only a Weak to the Context, and later callbacks
        // deref it — hold the Context alive alongside for the session.
        let core = context.connect(None)?;

        // A broken pipe (server gone) flips `done` so the supervisor can
        // reconnect.
        {
            let done = &flags.done;
            core.add_listener(pipewire_native::core::CoreEvents::new(
                None,
                None,
                some_closure!([^(done)] _id, _seq, res, _msg, {
                    if std::io::Error::from_raw_os_error(res as i32).kind()
                        == std::io::ErrorKind::BrokenPipe
                    {
                        done.store(true, Ordering::Relaxed);
                    }
                }),
            ));
        }

        let registry = core.registry()?;
        let reg_for_bind = registry.clone();
        registry.add_listener(RegistryEvents {
            global: some_closure!([reg_for_bind ^(tracker, client, flags)] id, _perms, type_, version, props, {
                if type_ != types::interface::NODE {
                    return;
                }
                // The global announcement carries only a few node keys:
                // `media.class`, but not `media.name`. So bind each video
                // output node and let its full info decide; most graph
                // nodes fail this class check and are never bound.
                if props.get(super::MEDIA_CLASS) != Some(super::SCREENCAST_MEDIA_CLASS) {
                    return;
                }
                watch_node(&reg_for_bind, id, version, tracker, client, flags);
            }),
            global_remove: some_closure!([^(tracker, client, flags)] id, {
                let reports = locked(tracker).on_removed(id);
                emit(client, flags, reports);
            }),
        });

        main_loop.run();
        // Park until a broken pipe or a dead core; poll cheaply so a
        // returning server is picked up without a persistent busy-wait.
        while !flags.done.load(Ordering::Relaxed) && !flags.core_gone.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        // Clear the source (real detail) before unwinding.
        let reports = locked(&tracker).clear();
        emit(client, &flags, reports);
        main_loop.quit();
        core.disconnect();
        drop(context);
        if flags.core_gone.load(Ordering::Relaxed) {
            return Err(FfiError::CoreGone);
        }
        Ok(())
    }

    /// Lock the tracker, recovering it if a panicking PipeWire callback
    /// poisoned the mutex: its state is plain bookkeeping that stays
    /// consistent between calls, so the next event can safely carry on.
    fn locked(tracker: &Mutex<Tracker>) -> MutexGuard<'_, Tracker> {
        tracker.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Bind a matching node and forward its run-state changes into the
    /// tracker as detail updates.
    /// Bind a candidate node: its info events (full properties first,
    /// then run state) feed the tracker, which starts tracking it only if
    /// the properties match.
    fn watch_node(
        registry: &pipewire_native::proxy::registry::Registry,
        id: u32,
        version: u32,
        tracker: &Arc<Mutex<Tracker>>,
        client: &RemoteClient,
        flags: &Flags,
    ) {
        let Ok(object) = registry.bind(id, types::interface::NODE, version) else {
            return;
        };
        let Some(node) = object.downcast::<pipewire_native::proxy::node::Node>() else {
            return;
        };
        node.add_listener(NodeEvents {
            info: some_closure!([^(tracker, client, flags)] info, {
                let mut np = node_props(info.props);
                np.state = Some(state_label(info.state).to_string());
                let reports = locked(tracker).on_info(info.id, &np);
                emit(client, flags, reports);
            }),
            ..Default::default()
        });
        // Keep the proxy alive for the node's lifetime; drop its state on
        // removal so a rebind after a re-add starts clean.
        node.proxy().add_listener(ProxyEvents {
            removed: some_closure!([node ^(tracker, client, flags)] {
                if let Some(id) = node.proxy().bound_id() {
                    let reports = locked(tracker).on_removed(id);
                    emit(client, flags, reports);
                }
            }),
            ..Default::default()
        });
    }

    /// Forward reports to the core; a dead core loop flips `core_gone` so
    /// the session unwinds and the supervisor stops.
    fn emit(client: &RemoteClient, flags: &Flags, reports: Vec<super::RemoteReport>) {
        for report in reports {
            if !client.report(report) {
                flags.core_gone.store(true, Ordering::Relaxed);
                flags.done.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(class: Option<&str>, name: Option<&str>) -> NodeProps {
        NodeProps {
            media_class: class.map(str::to_string),
            media_name: name.map(str::to_string),
            state: None,
        }
    }

    const OUTPUT_CAST: &str = "kwin-screencast-HDMI-A-1";

    fn kwin() -> NodeProps {
        props(Some(SCREENCAST_MEDIA_CLASS), Some(OUTPUT_CAST))
    }

    #[test]
    fn predicate_matches_only_kwin_video_output() {
        assert!(is_screencast(&kwin()));
        // Every cast type shares the prefix: outputs, virtual outputs,
        // windows (an app without a desktop file leaves the suffix empty)
        // and regions.
        for name in [
            "kwin-screencast-Virtual-krdp",
            "kwin-screencast-org.kde.konsole",
            "kwin-screencast-",
            "kwin-screencast-0,0 1920x1080",
        ] {
            assert!(is_screencast(&props(
                Some(SCREENCAST_MEDIA_CLASS),
                Some(name)
            )));
        }
        // Wrong class (an audio stream, a sink).
        assert!(!is_screencast(&props(
            Some("Stream/Input/Audio"),
            Some(OUTPUT_CAST)
        )));
        assert!(!is_screencast(&props(
            Some("Audio/Sink"),
            Some(OUTPUT_CAST)
        )));
        // Right class, another producer (Mutter's casts, a webcam, an app
        // streaming itself).
        for name in [
            "meta-stream-source-source",
            "v4l2",
            "Firefox",
            "kwin_wayland",
        ] {
            assert!(!is_screencast(&props(
                Some(SCREENCAST_MEDIA_CLASS),
                Some(name)
            )));
        }
        // Missing either property never matches.
        assert!(!is_screencast(&props(Some(SCREENCAST_MEDIA_CLASS), None)));
        assert!(!is_screencast(&props(None, Some(OUTPUT_CAST))));
        assert!(!is_screencast(&props(None, None)));
        // The state field is irrelevant to the predicate.
        let mut suspended = kwin();
        suspended.state = Some("suspended".into());
        assert!(is_screencast(&suspended));
    }

    /// The first info event's full properties decide; later events (which
    /// may carry no properties) only refresh a tracked node's state, and a
    /// non-matching node never starts tracking.
    #[test]
    fn info_events_track_by_full_properties() {
        let mut t = Tracker::default();
        let mut first = kwin();
        first.state = Some("idle".into());
        let r = t.on_info(1, &first);
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert_eq!(r[0].detail, "1 node (idle)");
        let later = NodeProps {
            state: Some("running".into()),
            ..NodeProps::default()
        };
        let r = t.on_info(1, &later);
        assert!(
            r.iter()
                .any(|report| report.active && report.detail == "1 node (running)")
        );
        let mut other = props(
            Some(SCREENCAST_MEDIA_CLASS),
            Some("meta-stream-source-source"),
        );
        other.state = Some("running".into());
        assert!(t.on_info(2, &other).is_empty());
        assert!(t.on_info(3, &later).is_empty());
    }

    #[test]
    fn summarize_is_deterministic_and_dedups() {
        assert_eq!(summarize(["running"].into_iter()), "1 node (running)");
        assert_eq!(
            summarize(["running", "idle"].into_iter()),
            "2 nodes (idle, running)"
        );
        // Order-independent + deduped.
        assert_eq!(
            summarize(["idle", "running"].into_iter()),
            summarize(["running", "idle"].into_iter())
        );
        assert_eq!(
            summarize(["running", "running"].into_iter()),
            "2 nodes (running)"
        );
        // An empty state label renders as `unknown`.
        assert_eq!(summarize([""].into_iter()), "1 node (unknown)");
    }

    #[test]
    fn appearance_arms_and_removal_disarms() {
        let mut t = Tracker::default();
        // A non-matching node is ignored entirely.
        assert!(t.on_global(1, &props(Some("Audio/Sink"), None)).is_empty());
        // A matching node (pre-state) arms immediately, active + detail.
        let r = t.on_global(2, &kwin());
        assert_eq!(r.len(), 1);
        assert!(r[0].active);
        assert_eq!(r[0].kind, RemoteSourceKind::Screencast);
        assert_eq!(r[0].detail, "1 node (unknown)");
        // Removing it disarms with a matching clear on the live detail.
        let r = t.on_removed(2);
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        assert_eq!(r[0].detail, "1 node (unknown)");
        // Removing an untracked id is a no-op.
        assert!(t.on_removed(99).is_empty());
    }

    #[test]
    fn state_change_replaces_detail_without_lapsing_the_source() {
        let mut t = Tracker::default();
        t.on_global(1, &kwin());
        // A state update swaps the detail: the new detail is reported
        // active BEFORE the stale one is cleared, so a wildcard `any()`
        // over the sources never dips (no spurious disconnect edge).
        let r = t.on_state(1, "running");
        assert_eq!(r.len(), 2);
        assert!(r[0].active);
        assert_eq!(r[0].detail, "1 node (running)");
        assert!(!r[1].active);
        assert_eq!(r[1].detail, "1 node (unknown)");
        // Re-reporting the same state is idempotent.
        assert!(t.on_state(1, "running").is_empty());
        // A state event for an untracked node is ignored.
        assert!(t.on_state(42, "running").is_empty());
    }

    #[test]
    fn second_node_updates_detail_but_stays_armed() {
        let mut t = Tracker::default();
        t.on_global(1, &kwin());
        assert!(t.on_state(1, "running").iter().any(|r| r.active));
        // A second capture node joins: detail grows, source stays live
        // (add-before-clear), never a disconnect.
        let r = t.on_global(2, &kwin());
        assert_eq!(r.len(), 2);
        assert!(r[0].active);
        assert_eq!(r[0].detail, "2 nodes (running, unknown)");
        assert!(!r[1].active);
        assert_eq!(r[1].detail, "1 node (running)");
        // Dropping one node shrinks the detail but the source persists.
        let r = t.on_removed(1);
        assert_eq!(r.len(), 2);
        assert!(r[0].active);
        assert_eq!(r[0].detail, "1 node (unknown)");
        // Dropping the last clears it.
        let r = t.on_removed(2);
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
    }

    #[test]
    fn clear_tears_down_the_live_source() {
        let mut t = Tracker::default();
        t.on_global(1, &kwin());
        t.on_state(1, "running");
        // A teardown clear (session ending / crash) emits the clear on
        // the real detail so nothing lingers armed.
        let r = t.clear();
        assert_eq!(r.len(), 1);
        assert!(!r[0].active);
        assert_eq!(r[0].detail, "1 node (running)");
        // Clearing an already-empty tracker is a no-op.
        assert!(t.clear().is_empty());
    }

    /// Candidates run baked dir first, then this architecture's multiarch
    /// dirs, then the fixed layouts, each once.
    #[test]
    fn spa_candidates_order_filter_and_dedup() {
        let usr_lib = [
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/i386-linux-gnu",
            "/usr/lib/firmware",
        ]
        .map(PathBuf::from);
        assert_eq!(
            spa_candidates(Some("/usr/lib64/spa-0.2"), usr_lib.clone(), "x86_64"),
            [
                "/usr/lib64/spa-0.2",
                "/usr/lib/x86_64-linux-gnu/spa-0.2",
                "/usr/lib/spa-0.2"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            spa_candidates(None, usr_lib, "x86_64"),
            [
                "/usr/lib/x86_64-linux-gnu/spa-0.2",
                "/usr/lib/spa-0.2",
                "/usr/lib64/spa-0.2"
            ]
            .map(PathBuf::from)
        );
    }

    /// An operator's pin is kept when it holds the plugin and reported
    /// when it doesn't; otherwise only candidates holding it are used, and
    /// none holding it reports what was searched.
    #[test]
    fn spa_plan_respects_the_pin_and_needs_the_plugin() {
        let good = PathBuf::from("/good");
        let has = |dir: &Path| dir == Path::new("/good");
        assert_eq!(
            plan_spa_dirs(Some(vec!["/bad".into(), good.clone()]), vec![], has).unwrap(),
            SpaPlan::KeepPinned
        );
        let missing = plan_spa_dirs(Some(vec!["/bad".into()]), vec![good.clone()], has)
            .expect_err("a pin without the plugin isn't overridden");
        assert_eq!(missing.searched, vec![PathBuf::from("/bad")]);
        assert_eq!(
            plan_spa_dirs(None, vec!["/bad".into(), good.clone()], has).unwrap(),
            SpaPlan::Point(vec![good])
        );
        let missing = plan_spa_dirs(None, vec!["/bad".into()], has)
            .expect_err("no candidate holds the plugin");
        assert_eq!(missing.searched, vec![PathBuf::from("/bad")]);
        assert!(missing.to_string().contains("SPA_PLUGIN_DIR"));
    }
}
