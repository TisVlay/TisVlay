//! Per-user session service: the gateway between the system daemon's
//! socket and a private per-user socket.
//!
//! Two zenoh sessions, no routing between them — that is the isolation
//! design. Session A (client → system socket) mirrors TV meta, publishes
//! session-event taps, and calls TV rpcs. Session B (peer listening on
//! the user socket, 0700 runtime dir) owns everything under
//! `tisvlay/<site>/user/**`: profile bodies, per-TV assignments, the rpc
//! queryable, and the service's liveliness token. User-private keys
//! physically never reach the system peer.

mod detector;
mod lock;
mod logind;
mod monitor;
mod remote;
mod screencast;
mod store;
pub mod sunshine;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tisvlay_core::{ResolvedUserProfile, SessionAction, UserProfileConfig};
use tisvlay_wire::caller::Caller;
use tisvlay_wire::error::WireError;
use tisvlay_wire::{keys, payload, rpc};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::query::Query;
use zenoh::sample::SampleKind;
use zenoh_ext::{
    AdvancedPublisher, AdvancedPublisherBuilderExt, AdvancedSubscriberBuilderExt, CacheConfig,
    HistoryConfig, MissDetectionConfig,
};

pub use lock::{seed_lock_state, seed_lock_state_on, spawn as spawn_lock_watcher, watch_lock};
pub use logind::spawn as spawn_logind_detector;
pub use monitor::spawn as spawn_portal_monitor;
pub use remote::RemoteReport;
pub use screencast::spawn as spawn_screencast_detector;
pub use store::{Assignment, UserProfileRecord, UserStoreSnapshot, load_user_store};
pub use sunshine::spawn as spawn_sunshine_detector;

/// Heartbeat period for sample-miss recovery on cached publishers.
const MISS_HEARTBEAT: Duration = Duration::from_millis(500);

/// A session-scoped desktop event, from whichever adapter saw it
/// (logind `LockedHint`, wayland/Mutter idle, or a test driver).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Lock,
    Unlock,
    /// First user activity of an idle cycle while the screen is locked —
    /// the panel comes back to show the password prompt.
    LockWake,
    IdleTimeout,
    IdleResume,
    /// A remote session began (the first source armed).
    RemoteConnect,
    /// The last remote session ended (all sources cleared).
    RemoteDisconnect,
    /// First genuine local input after a remote session ended — the panel
    /// the disconnect blank left dark comes back for the returning person.
    /// Deliberately not an arming edge: it flows through the while-remote
    /// gate, which stays load-bearing because a TV can re-arm via a
    /// profile/meta recompute (no `RemoteConnect` edge) while the return
    /// watch is still set.
    LocalReturn,
}

impl SessionEvent {
    /// The tap key name under `event/session/`.
    fn tap_name(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::LockWake => "lock_wake",
            Self::IdleTimeout => "idle",
            Self::IdleResume => "resume",
            Self::RemoteConnect => "remote_connect",
            Self::RemoteDisconnect => "remote_disconnect",
            Self::LocalReturn => "local_return",
        }
    }

    /// The connect/disconnect edges. They dispatch at the arming boundary
    /// itself, so they bypass the while-remote gate (honoring it would
    /// swallow the very action the edge exists to fire) and stay orthogonal
    /// to the idle-drift window.
    fn is_remote_edge(self) -> bool {
        matches!(self, Self::RemoteConnect | Self::RemoteDisconnect)
    }

    /// Seat-local presence events (lock + idle) — the ones subject to
    /// active-session arbitration, as opposed to the seatless remote
    /// edges, which are governed by the while-remote gate instead.
    fn is_seat_local(self) -> bool {
        matches!(
            self,
            Self::Lock | Self::Unlock | Self::LockWake | Self::IdleTimeout | Self::IdleResume
        )
    }
}

/// Handle for injecting session events — adapters and tests alike.
#[derive(Debug, Clone)]
pub struct SessionClient {
    tx: mpsc::UnboundedSender<SessionEvent>,
}

impl SessionClient {
    /// Returns false if the core loop is gone.
    pub fn send(&self, event: SessionEvent) -> bool {
        self.tx.send(event).is_ok()
    }
}

/// Handle for injecting remote-source reports — the detector-report
/// seam. Real detectors (spawned in `main.rs`) and the test Rig both
/// report through it; the verb rpc served on the user socket feeds the
/// same channel from inside the core.
#[derive(Debug, Clone)]
pub struct RemoteClient {
    tx: mpsc::UnboundedSender<RemoteReport>,
}

impl RemoteClient {
    /// Returns false if the core loop is gone.
    pub fn report(&self, report: RemoteReport) -> bool {
        self.tx.send(report).is_ok()
    }
}

#[derive(Debug)]
pub struct SessionOptions {
    pub system_socket: PathBuf,
    pub user_socket: PathBuf,
    pub site: String,
    /// Persisted user profiles + assignments. `None` = ephemeral.
    pub store: Option<PathBuf>,
    /// The Sunshine observation the core's eager pull takes on a gated
    /// wake — [`sunshine::HostProbe`] in production, scripted in tests.
    pub sunshine: Arc<dyn sunshine::SunshineProbe>,
}

/// A running session service.
#[derive(Debug)]
pub struct SessionStack {
    pub client: SessionClient,
    /// Detector-report seam for remote-session sources — `main.rs`'s
    /// (future) real detectors and the test Rig both report through it.
    pub remote: RemoteClient,
    /// Resolved user idle timeouts as `(locked, unlocked)` seconds —
    /// each the min enabled across automated TVs, 0 = disabled — the
    /// wayland idle adapter re-registers on change.
    pub idle_timeouts: watch::Receiver<(u64, u64)>,
    /// Shared lock state: the lock watcher writes it (`LockedHint` seed +
    /// edges), the idle adapter's lock-wake policy and the core's
    /// drift re-assert read it.
    pub is_locked: Arc<AtomicBool>,
    /// Shared foreground state: the lock watcher writes it (the session's
    /// `Active` seed + changes), the core's active-session arbitration gate
    /// reads it. Defaults `true` (assume active until logind says
    /// otherwise), so a non-logind host never suppresses automation.
    pub is_active: Arc<AtomicBool>,
    /// Set on a remote disconnect, cleared by the next dispatched event —
    /// handed to the idle adapter so the first local input after a remote
    /// session wakes the panel the disconnect left dark.
    pub local_return_armed: Arc<AtomicBool>,
}

/// Boot the session service: open both sessions, replay the store, and
/// run the core loop for the process lifetime.
pub async fn serve(options: SessionOptions) -> Result<SessionStack> {
    let system = zenoh::open(
        tisvlay_wire::bootstrap::client_config_resilient(&options.system_socket)
            .context("system session config")?,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "connect to system socket {}: {e}",
            options.system_socket.display()
        )
    })?;
    if let Some(parent) = options.user_socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create user socket dir {}", parent.display()))?;
    }
    let user = zenoh::open(
        tisvlay_wire::bootstrap::router_config(&options.user_socket)
            .context("user session config")?,
    )
    .await
    .map_err(|e| {
        anyhow!(
            "listen on user socket {}: {e}",
            options.user_socket.display()
        )
    })?;
    let mut snapshot = match &options.store {
        Some(path) => load_user_store(path)?,
        None => UserStoreSnapshot::default(),
    };
    // Root ensure_default parity: the editable default profile exists
    // from first boot; stored edits to it survive (seed only if absent),
    // and its name stays pinned.
    if !snapshot
        .profile
        .iter()
        .any(|record| record.id == tisvlay_core::DEFAULT_USER_PROFILE_UUID)
    {
        let mut body = UserProfileConfig::full_default();
        body.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
        snapshot.profile.push(UserProfileRecord {
            id: tisvlay_core::DEFAULT_USER_PROFILE_UUID,
            body,
        });
    }
    for record in &mut snapshot.profile {
        if record.id == tisvlay_core::DEFAULT_USER_PROFILE_UUID {
            record.body.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
        }
    }
    let persist = options.store.as_ref().map(|path| {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(store::store_task(path.clone(), rx));
        tx
    });

    // RPC queries land in the core loop through a channel, like the
    // daemon bridge — one task owns all state, no locks.
    let (query_tx, queries) = mpsc::unbounded_channel::<Query>();
    let queryable = user
        .declare_queryable(keys::user_profile_rpc_wild(&options.site))
        .complete(true)
        .callback(move |query| {
            let _ = query_tx.send(query);
        })
        .await
        .map_err(|e| anyhow!("declare user rpc queryable: {e}"))?;

    // Remote-session rpcs on their own queryable family (verb rpc for
    // manual/hook sources); routed into the same core loop.
    let (remote_query_tx, remote_queries) = mpsc::unbounded_channel::<Query>();
    let remote_queryable = user
        .declare_queryable(keys::user_remote_rpc_wild(&options.site))
        .complete(true)
        .callback(move |query| {
            let _ = remote_query_tx.send(query);
        })
        .await
        .map_err(|e| anyhow!("declare remote rpc queryable: {e}"))?;

    // Liveliness after the rpc queryables: a client that observes the
    // service alive must find every rpc surface already declared.
    let alive = user
        .liveliness()
        .declare_token(keys::user_alive_session(&options.site))
        .await
        .map_err(|e| anyhow!("declare session liveliness: {e}"))?;

    // Fleet mirror: every TV's meta/config, hydrated from history.
    let meta_sub = system
        .declare_subscriber(keys::tv_meta_wild(&options.site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe tv meta: {e}"))?;

    // Screen mirror: per-TV power state, for the idle drift re-assert
    // (a panel lit by a stray remote press while the idle action holds).
    let power_sub = system
        .declare_subscriber(keys::tv_power_wild(&options.site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe tv power: {e}"))?;

    // The remote-session mirror the daemon hydrates on restart: a cache-1
    // AdvancedPublisher on the system link. Declared before the presence
    // token so the daemon reaches its `@adv` cache the moment it observes
    // the session alive. Served over a connect-only peer, but the
    // directly-dialed daemon still hydrates it — the counter-sequenced
    // cache shape constructs without an HLC, so no timestamping is needed
    // here.
    let remote_mirror = system
        .declare_publisher(keys::remote_session(&options.site))
        .encoding(Encoding::APPLICATION_JSON)
        .cache(CacheConfig::default().max_samples(1))
        .sample_miss_detection(MissDetectionConfig::default().heartbeat(MISS_HEARTBEAT))
        .publisher_detection()
        .await
        .map_err(|e| anyhow!("declare remote mirror publisher: {e}"))?;

    // Presence on the system socket: a liveliness token the daemon (the
    // directly-dialed peer) observes even though this link is a
    // connect-only peer. Its DELETE — within a millisecond of a crash or a
    // clean exit — is the daemon's crash-robust departure signal, clearing
    // any armed state this session had mirrored. Held in `Core` so it lives
    // exactly as long as the service; the user-socket `user_alive_session`
    // token never reaches the system peer, so presence rides this one.
    let alive_system = system
        .liveliness()
        .declare_token(keys::session_alive(&options.site))
        .await
        .map_err(|e| anyhow!("declare session system liveliness: {e}"))?;

    let (event_tx, events) = mpsc::unbounded_channel();
    let (remote_tx, reports) = mpsc::unbounded_channel();
    let (idle_tx, idle_rx) = watch::channel((0u64, 0u64));
    let is_locked = Arc::new(AtomicBool::new(false));
    // Default true: assume the user is active until the lock watcher seeds
    // the real `Active` state, so startup and non-logind hosts never suppress.
    let is_active = Arc::new(AtomicBool::new(true));
    let local_return_armed = Arc::new(AtomicBool::new(false));

    // Matching-gated caller for TV rpcs on the system socket, shared into
    // every dispatch's spawned task.
    let caller = Arc::new(Caller::new(system.clone()));

    let mut core = Core {
        system,
        user,
        sunshine: options.sunshine,
        site: options.site,
        profiles: snapshot
            .profile
            .into_iter()
            .map(|record| (record.id, record.body))
            .collect(),
        assignments: snapshot
            .assignment
            .into_iter()
            .map(|assignment| (assignment.tv, assignment.profile))
            .collect(),
        tvs: HashMap::new(),
        publishers: HashMap::new(),
        persist,
        state_path: options
            .store
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        idle_tx,
        is_locked: is_locked.clone(),
        is_active: is_active.clone(),
        local_return_armed: local_return_armed.clone(),
        idle_fired: false,
        screen_on: HashMap::new(),
        drift_at: HashMap::new(),
        remote_sources: remote::RemoteSources::default(),
        remote_armed: HashSet::new(),
        remote_mirror,
        caller,
        _alive: alive,
        _alive_system: alive_system,
    };
    tokio::spawn(async move {
        core.run(
            events,
            reports,
            queries,
            remote_queries,
            meta_sub,
            power_sub,
        )
        .await;
        drop(queryable);
        drop(remote_queryable);
    });

    Ok(SessionStack {
        client: SessionClient { tx: event_tx },
        remote: RemoteClient { tx: remote_tx },
        idle_timeouts: idle_rx,
        is_locked,
        is_active,
        local_return_armed,
    })
}

struct Core {
    system: Session,
    user: Session,
    /// The eager pull's Sunshine observation (see `eager_refresh_sunshine`).
    sunshine: Arc<dyn sunshine::SunshineProbe>,
    site: String,
    profiles: HashMap<Uuid, UserProfileConfig>,
    assignments: HashMap<Uuid, Uuid>,
    tvs: HashMap<Uuid, payload::Meta>,
    publishers: HashMap<String, AdvancedPublisher<'static>>,
    persist: Option<mpsc::UnboundedSender<UserStoreSnapshot>>,
    /// For the `info` rpc (About pane row).
    state_path: String,
    idle_tx: watch::Sender<(u64, u64)>,
    /// Shared with the adapters; the drift re-assert reads it to pick
    /// the locked vs unlocked grace (a core-local flag would miss the
    /// `LockedHint`-seeded auto-login lock, which arrives with no edge).
    is_locked: Arc<AtomicBool>,
    /// The session's foreground (`Active`) state, written by the lock
    /// watcher. `dispatch` gates seat-local automation on it so a
    /// backgrounded (fast-switched-away) user does not drive TVs the active
    /// user is using.
    is_active: Arc<AtomicBool>,
    /// Set on a remote disconnect, cleared by the next dispatched event.
    /// The idle adapter reads it so the first local input after a remote
    /// session wakes the panel the disconnect left dark (`LocalReturn`).
    local_return_armed: Arc<AtomicBool>,
    /// An idle action fired and no activity has resumed since — the
    /// window in which a self-lighting panel counts as drift.
    idle_fired: bool,
    /// Mirrored `state/power` screen bit per TV.
    screen_on: HashMap<Uuid, bool>,
    /// Armed drift re-asserts: TV → deadline (one idle-timeout of grace
    /// from the drift; a deliberate remote press gets its window).
    drift_at: HashMap<Uuid, tokio::time::Instant>,
    /// Live remote-session sources (the detector aggregate).
    remote_sources: remote::RemoteSources,
    /// TVs the current sources arm, per their resolved profile — the
    /// while-remote gate in `action_for` keys on membership.
    remote_armed: HashSet<Uuid>,
    /// Cache-1 mirror of the remote fact on the system link. The daemon
    /// hydrates its last sample on restart, so the fact survives a daemon
    /// bounce without any re-put from the session.
    remote_mirror: AdvancedPublisher<'static>,
    /// Matching-gated caller for TV rpcs on the system socket.
    caller: Arc<Caller>,
    _alive: zenoh::liveliness::LivelinessToken,
    /// System-socket presence token. Dropping it (Core drop, on crash or
    /// clean exit) sends the daemon its departure signal, a liveliness
    /// DELETE; held here so it lives exactly as long as the running service.
    _alive_system: zenoh::liveliness::LivelinessToken,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        &mut self,
        mut events: mpsc::UnboundedReceiver<SessionEvent>,
        mut reports: mpsc::UnboundedReceiver<RemoteReport>,
        mut queries: mpsc::UnboundedReceiver<Query>,
        mut remote_queries: mpsc::UnboundedReceiver<Query>,
        meta_sub: zenoh_ext::AdvancedSubscriber<
            zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>,
        >,
        power_sub: zenoh_ext::AdvancedSubscriber<
            zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>,
        >,
    ) {
        // Late joiners hydrate the catalog from these caches.
        for (id, body) in self.profiles.clone() {
            self.publish_profile(&id, &body).await;
        }
        for (tv, profile) in self.assignments.clone() {
            self.publish_assignment(&tv, Some(profile)).await;
        }
        self.recompute_idle();
        // Seed the remote fact (empty sources / empty armed) so late
        // joiners hydrate a defined state.
        self.publish_remote().await;
        info!(site = %self.site, "session core running");
        'run: loop {
            let next_drift = self.drift_at.values().min().copied();
            tokio::select! {
                event = events.recv() => match event {
                    None => break 'run,
                    Some(event) => {
                        // Pre-dispatch: a wake-ish gated event with a live
                        // Sunshine source refreshes it on a tight budget so
                        // the gate reads fresh state (a bounded await in this
                        // single-owner loop — see the method).
                        self.eager_refresh_sunshine(event).await;
                        self.dispatch(event).await;
                    }
                },
                report = reports.recv() => match report {
                    None => break 'run,
                    Some(report) => self.apply_remote_report(report).await,
                },
                query = queries.recv() => match query {
                    None => break 'run,
                    Some(query) => self.handle_query(query).await,
                },
                query = remote_queries.recv() => match query {
                    None => break 'run,
                    Some(query) => self.handle_remote_query(query).await,
                },
                sample = meta_sub.recv_async() => match sample {
                    Err(_) => break 'run,
                    Ok(sample) => self.apply_meta(&sample).await,
                },
                sample = power_sub.recv_async() => match sample {
                    Err(_) => break 'run,
                    Ok(sample) => self.apply_power(&sample),
                },
                _ = drift_deadline(next_drift) => self.reassert_due(),
            }
        }
        // Clean exit (a channel closed on logout/restart): the
        // `_alive_system` token, dropped with `Core` immediately after,
        // sends a DELETE to the daemon's `session_alive` subscriber, which
        // clears the armed set. A crash that skips this path departs the same
        // token, so the daemon disarms on the DELETE either way — no empty
        // mirror put is needed.
    }

    async fn apply_meta(&mut self, sample: &zenoh::sample::Sample) {
        let Some((tv, "meta/config")) = keys::parse_tv_path(&self.site, sample.key_expr().as_str())
        else {
            return;
        };
        match sample.kind() {
            SampleKind::Put => {
                match serde_json::from_slice::<payload::Meta>(&sample.payload().to_bytes()) {
                    Ok(meta) => {
                        self.tvs.insert(tv, meta);
                    }
                    Err(e) => warn!(%tv, %e, "undecodable tv meta"),
                }
            }
            SampleKind::Delete => {
                self.tvs.remove(&tv);
            }
        }
        self.recompute_idle();
        // The fleet changed — a new TV (or a profile shift reflected in
        // its meta) can move the armed set. TV hydration can postdate a
        // detector report on a cold start (the system link's first connect
        // may fail and reconnect after a source is already live), so this
        // is the edge that arms a just-learned TV.
        if self.recompute_remote_armed() {
            self.publish_remote().await;
        }
    }

    fn apply_power(&mut self, sample: &zenoh::sample::Sample) {
        let Some((tv, "state/power")) = keys::parse_tv_path(&self.site, sample.key_expr().as_str())
        else {
            return;
        };
        match sample.kind() {
            SampleKind::Put => {
                match serde_json::from_slice::<payload::Power>(&sample.payload().to_bytes()) {
                    Ok(power) => {
                        let was = self.screen_on.insert(tv, power.is_screen_on);
                        if power.is_screen_on && was != Some(true) {
                            self.arm_drift(tv);
                        } else if !power.is_screen_on {
                            // Back in compliance — cancel any pending
                            // re-assert.
                            self.drift_at.remove(&tv);
                        }
                    }
                    Err(e) => warn!(%tv, %e, "undecodable tv power"),
                }
            }
            SampleKind::Delete => {
                self.screen_on.remove(&tv);
                self.drift_at.remove(&tv);
            }
        }
    }

    /// A panel lit itself while an idle action holds (a stray remote
    /// press at the lock screen): give it one idle-timeout of grace,
    /// then re-assert the idle action. Gated exactly like the original
    /// dispatch — `automated` carries the automate_when/primary-input
    /// policy — and only dark intents re-assert.
    fn arm_drift(&mut self, tv: Uuid) {
        if !self.idle_fired {
            return;
        }
        if !self.tvs.get(&tv).is_some_and(|m| m.automated) {
            return;
        }
        let resolved = self.resolve_for(&tv);
        if !matches!(
            resolved.on_idle_timeout.action(),
            SessionAction::ScreenOff | SessionAction::PowerOff
        ) {
            return;
        }
        let grace = if self.is_locked.load(Ordering::SeqCst) {
            resolved.locked_idle_timeout_seconds
        } else {
            resolved.unlocked_idle_timeout_seconds
        };
        if grace == 0 {
            return;
        }
        debug!(%tv, grace, "screen drift while idle; re-assert armed");
        self.drift_at
            .insert(tv, tokio::time::Instant::now() + Duration::from_secs(grace));
    }

    /// Fire due drift re-asserts: still idle, still lit, still automated
    /// → re-dispatch the idle action through the normal rpc rail.
    fn reassert_due(&mut self) {
        let now = tokio::time::Instant::now();
        let due: Vec<Uuid> = self
            .drift_at
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(tv, _)| *tv)
            .collect();
        for tv in due {
            self.drift_at.remove(&tv);
            if !self.idle_fired
                || self.screen_on.get(&tv) != Some(&true)
                || !self.tvs.get(&tv).is_some_and(|m| m.automated)
            {
                continue;
            }
            let action = self.resolve_for(&tv).on_idle_timeout;
            debug!(%tv, ?action, "re-asserting idle action after screen drift");
            self.call_tv(tv, action);
        }
    }

    /// Apply a detector report: fold it into the source set, recompute
    /// the armed projection, publish the fact once, and drive the
    /// connect/disconnect edge on the armed-set transition.
    async fn apply_remote_report(&mut self, report: RemoteReport) {
        if !self.remote_sources.apply(&report) {
            return;
        }
        debug!(?report, "remote source report applied");
        // Edge on the ARMED-SET transition, not raw source presence: an
        // unarmed source (a screencast with capture-arming off, OBS or a
        // browser tab-share) must not fire the connect action and blank
        // every TV. Any live-set change republishes the fact, but only a
        // move of the armed set drives the connect/disconnect events.
        let was_armed = !self.remote_armed.is_empty();
        self.recompute_remote_armed();
        self.publish_remote().await;
        let now_armed = !self.remote_armed.is_empty();
        match (was_armed, now_armed) {
            (false, true) => self.dispatch(SessionEvent::RemoteConnect).await,
            (true, false) => self.dispatch(SessionEvent::RemoteDisconnect).await,
            _ => {}
        }
    }

    /// Recompute the armed set from the live source kinds crossed with each
    /// known TV's resolved profile. A pure state update: it moves
    /// `remote_armed` and returns whether the set changed, publishing
    /// nothing — every caller publishes exactly once on a change. With no
    /// live source nothing can arm, so the empty-source case clears the set
    /// and skips resolving any profile (hydration replays this per meta
    /// sample).
    fn recompute_remote_armed(&mut self) -> bool {
        if !self.remote_sources.any() {
            if self.remote_armed.is_empty() {
                return false;
            }
            self.remote_armed.clear();
            return true;
        }
        let kinds = self.remote_sources.kinds();
        let armed: HashSet<Uuid> =
            remote::compute_armed(self.tvs.keys(), &kinds, |tv| self.resolve_for(tv))
                .into_iter()
                .collect();
        if armed != self.remote_armed {
            self.remote_armed = armed;
            true
        } else {
            false
        }
    }

    /// Publish the fact both ways: cached pub on the user socket, plain
    /// mirror on the system socket. The `RemoteSessionState` is built once
    /// and reused for both puts.
    async fn publish_remote(&mut self) {
        let state = self.remote_state();
        let key = keys::user_remote_session(&self.site);
        match serde_json::to_value(&state) {
            Ok(payload) => self.publish(&key, &payload).await,
            Err(e) => warn!(%e, "remote session serialize failed"),
        }
        self.mirror_state(&state).await;
    }

    /// Publish a fact to the system-socket mirror through the cache-1
    /// AdvancedPublisher. The directly-dialed daemon hydrates its last
    /// sample on restart, so the fact survives a daemon bounce without a
    /// re-put; a live daemon receives it as an ordinary sample.
    async fn mirror_state(&self, state: &payload::RemoteSessionState) {
        let bytes = match serde_json::to_vec(state) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(%e, "remote mirror serialize failed");
                return;
            }
        };
        if let Err(e) = self.remote_mirror.put(bytes).await {
            warn!(%e, "remote mirror put failed");
        }
    }

    /// The current fact: live sources + the armed projection (sorted).
    fn remote_state(&self) -> payload::RemoteSessionState {
        let mut armed: Vec<Uuid> = self.remote_armed.iter().copied().collect();
        armed.sort();
        payload::RemoteSessionState {
            sources: self.remote_sources.snapshot(),
            armed,
        }
    }

    /// Serve the remote verb rpc: `set { source, active }` toggles a
    /// manual source; `status` returns the current fact.
    async fn handle_remote_query(&mut self, query: Query) {
        let key = query.key_expr().as_str().to_string();
        let Some(verb) = keys::parse_user_remote_rpc(&self.site, &key).map(str::to_string) else {
            reply_err(
                &query,
                &WireError::bad_request(format!("no such rpc: {key}")),
            )
            .await;
            return;
        };
        let body: Value = query
            .payload()
            .and_then(|bytes| serde_json::from_slice(&bytes.to_bytes()).ok())
            .unwrap_or(Value::Null);
        match verb.as_str() {
            "set" => {
                // Default source `manual`; `active` required.
                let source = body
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("manual")
                    .to_string();
                let Some(active) = body.get("active").and_then(Value::as_bool) else {
                    reply_err(&query, &WireError::bad_request("set requires `active`")).await;
                    return;
                };
                self.apply_remote_report(RemoteReport::manual(source, active))
                    .await;
                reply_json(
                    &query,
                    &serde_json::to_value(self.remote_state()).unwrap_or_default(),
                )
                .await;
            }
            "status" => {
                reply_json(
                    &query,
                    &serde_json::to_value(self.remote_state()).unwrap_or_default(),
                )
                .await;
            }
            other => {
                reply_err(
                    &query,
                    &WireError::bad_request(format!("no such rpc: {other}")),
                )
                .await;
            }
        }
    }

    /// Eager Sunshine refresh, run before an event whose dispatch the gate
    /// is currently suppressing. The continuous poll disarms the Sunshine
    /// source only within its interval, so a user returning inside that
    /// window could hit a suppressed dispatch for up to one tick. The gate
    /// and the detector share this process, so a suppressed dispatch pulls
    /// `/serverinfo` synchronously and decides on fresh state: if the stream
    /// has ended the source clears here, un-gating the action immediately
    /// instead of waiting for the poll.
    ///
    /// Bounded: awaits up to `EAGER_TIMEOUT` in the single-owner Core loop,
    /// and only when a live Sunshine source is holding down a real dispatch
    /// (see `eager_refresh_applies`) — infrequent and tight, which is why the
    /// budget is small and the trigger narrow. It refreshes the source only;
    /// the connect/disconnect edge accounting stays with the poll and the
    /// report path.
    async fn eager_refresh_sunshine(&mut self, event: SessionEvent) {
        let has_sunshine = self
            .remote_sources
            .has_kind(payload::RemoteSourceKind::Sunshine);
        let armed_actions: Vec<tisvlay_core::EventAction> = self
            .remote_armed
            .iter()
            .map(|tv| self.raw_action_for(tv, event))
            .collect();
        if !eager_refresh_applies(event, has_sunshine, armed_actions.into_iter()) {
            return;
        }
        // The poll owns the single (Sunshine, "serverinfo") key and it is
        // live here (checked above), so fold the observation against
        // armed = true: Free/Absent clears the source, Busy/Unknown leaves it.
        let observation = self.sunshine.observe(sunshine::EAGER_TIMEOUT).await;
        if let (_, Some(report)) = sunshine::decide(true, observation)
            && self.remote_sources.apply(&report)
        {
            self.recompute_remote_armed();
            self.publish_remote().await;
        }
    }

    /// Session event → per-TV user-profile action, called through the
    /// system daemon's rpcs. The tap publish comes first so observers see
    /// the cause before the effects.
    async fn dispatch(&mut self, event: SessionEvent) {
        // One observability point for every session event (lock, idle,
        // remote) as it enters the core — the audit seam across transports.
        debug!(?event, "session event");
        // Idle-drift bookkeeping: an idle action opens the drift window,
        // any activity or lock transition closes it (a fresh idle cycle
        // will re-open it through its own timer).
        match event {
            SessionEvent::IdleTimeout => self.idle_fired = true,
            // Remote connect/disconnect are orthogonal to the idle-drift
            // window — they neither open nor close it.
            _ if event.is_remote_edge() => {}
            // Any activity or lock transition closes the drift window (a
            // fresh idle cycle re-opens it through its own timer).
            _ => {
                self.idle_fired = false;
                self.drift_at.clear();
            }
        }
        // Post-remote return watch: a disconnect arms it (the next local
        // presence pulse wakes the panel the disconnect left dark); every
        // other event supersedes and disarms it. The idle adapter reads it.
        self.local_return_armed
            .store(event == SessionEvent::RemoteDisconnect, Ordering::SeqCst);
        let tap = keys::session_event(&self.site, event.tap_name());
        if let Err(e) = self
            .system
            .put(tap.as_str(), b"{}".to_vec())
            .encoding(Encoding::APPLICATION_JSON)
            .await
        {
            warn!(%tap, %e, "session tap publish failed");
        }
        // Active-session arbitration: while this user's session is not the
        // seat's foreground, suppress its seat-local automation so a
        // backgrounded (fast-switched-away) user cannot drive TVs the active
        // user is using. Absolute — no off-going/on-going exception, since a
        // backgrounded off would disrupt a shared TV's active user. Remote
        // edges are a separate, seatless axis and keep their while-remote gate.
        // The audit tap above still fires; only the automation is suppressed.
        if event.is_seat_local() && !self.is_active.load(Ordering::SeqCst) {
            debug!(?event, "automation suppressed (session not active)");
            return;
        }
        for (tv, meta) in &self.tvs {
            if !meta.automated {
                debug!(%tv, ?event, "automation gated off");
                continue;
            }
            let action = self.action_for(tv, event);
            if action == SessionAction::None {
                continue;
            }
            debug!(%tv, ?event, ?action, "user profile action");
            self.call_tv(*tv, action);
        }
    }

    /// Fire a TV rpc on the system socket through the shared matching gate;
    /// failures are logged, not fatal — the daemon's own retry tier owns
    /// delivery. The resolved restore flag rides the rpc as resolved intent:
    /// the world honors it without knowing the session's event vocabulary,
    /// and manual clients (which never set it) stay input-neutral.
    fn call_tv(&self, tv: Uuid, spec: tisvlay_core::EventAction) {
        let restore = spec.restore_input();
        let (verb, body) = match spec.action() {
            SessionAction::ScreenOff => ("set-screen", json!({ "on": false })),
            SessionAction::ScreenOn => (
                "set-screen",
                json!({ "on": true, "restore_input": restore }),
            ),
            SessionAction::PowerOff => ("power-off", Value::Null),
            SessionAction::PowerOn => ("power-on", json!({ "restore_input": restore })),
            SessionAction::None => return,
        };
        let caller = Arc::clone(&self.caller);
        let key = keys::tv_rpc(&self.site, &tv, verb);
        // Spawned so the matching gate + rpc timeout never block the core
        // select; the Caller re-issues nothing on a write and classifies
        // unavailable-vs-rejected for the log line.
        tokio::spawn(async move {
            if let Err(error) = caller.call(&key, body).await {
                warn!(%key, %error, "tv rpc failed");
            }
        });
    }

    /// The resolved action for this event *before* the remote gate: the
    /// root chain per field (assignment → profile → editable default
    /// profile → hard-coded floor). One field per event, no direction bias
    /// — an event carries no inherent wake/off-going meaning, only the
    /// profile it resolves through does.
    fn raw_action_for(&self, tv: &Uuid, event: SessionEvent) -> tisvlay_core::EventAction {
        let resolved = self.resolve_for(tv);
        match event {
            SessionEvent::Lock => resolved.on_session_lock,
            SessionEvent::Unlock => resolved.on_session_unlock,
            SessionEvent::LockWake => resolved.on_lock_wake,
            SessionEvent::IdleTimeout => resolved.on_idle_timeout,
            SessionEvent::IdleResume => resolved.on_idle_resume,
            SessionEvent::RemoteConnect => resolved.on_remote_connect,
            SessionEvent::RemoteDisconnect => resolved.on_remote_disconnect,
            SessionEvent::LocalReturn => resolved.on_local_return,
        }
    }

    /// The dispatched action: the raw mapping with the remote gate applied.
    /// While a TV is armed, an action whose `while_remote` gate is `false`
    /// is suppressed (dispatches nothing) — a pure gate, so an unarmed TV is
    /// never affected and the gate can never originate an action. The gate
    /// is ignored on the connect/disconnect events (those fire at the arming
    /// edge itself, where honoring the gate would swallow the action).
    fn action_for(&self, tv: &Uuid, event: SessionEvent) -> tisvlay_core::EventAction {
        let action = self.raw_action_for(tv, event);
        if !event.is_remote_edge() && self.remote_armed.contains(tv) && !action.while_remote() {
            return SessionAction::None.into();
        }
        action
    }

    /// Merge an assignment through the editable default profile down to
    /// the hard-coded floor (root resolve chain).
    fn resolve_for(&self, tv: &Uuid) -> ResolvedUserProfile {
        // Merge off borrowed bodies — `merge_with_base`/`resolve` take refs,
        // so an absent assignment or default profile falls back to a shared
        // empty rather than cloning whole config bodies (this runs per TV on
        // every arming recompute).
        let empty = UserProfileConfig::default();
        let assigned = self
            .assignments
            .get(tv)
            .filter(|profile| **profile != tisvlay_core::DEFAULT_USER_PROFILE_UUID)
            .and_then(|profile| self.profiles.get(profile))
            .unwrap_or(&empty);
        let default_body = self
            .profiles
            .get(&tisvlay_core::DEFAULT_USER_PROFILE_UUID)
            .unwrap_or(&empty);
        assigned.merge_with_base(default_body).resolve()
    }

    /// Shortest enabled idle timeouts (locked, unlocked) across
    /// automated TVs' resolved profiles — the wayland adapter's
    /// registration values.
    fn recompute_idle(&self) {
        let mut timeouts = (0u64, 0u64);
        let merge = |slot: &mut u64, seconds: u64| {
            if seconds > 0 && (*slot == 0 || seconds < *slot) {
                *slot = seconds;
            }
        };
        for (tv, meta) in &self.tvs {
            if !meta.automated {
                continue;
            }
            let resolved = self.resolve_for(tv);
            merge(&mut timeouts.0, resolved.locked_idle_timeout_seconds);
            merge(&mut timeouts.1, resolved.unlocked_idle_timeout_seconds);
        }
        self.idle_tx.send_if_modified(|current| {
            let changed = *current != timeouts;
            *current = timeouts;
            changed
        });
    }

    fn snapshot(&self) -> UserStoreSnapshot {
        UserStoreSnapshot {
            version: UserStoreSnapshot::default().version,
            profile: self
                .profiles
                .iter()
                .map(|(id, body)| UserProfileRecord {
                    id: *id,
                    body: body.clone(),
                })
                .collect(),
            assignment: self
                .assignments
                .iter()
                .map(|(tv, profile)| Assignment {
                    tv: *tv,
                    profile: *profile,
                })
                .collect(),
        }
    }

    fn persist(&self) {
        if let Some(tx) = &self.persist {
            let _ = tx.send(self.snapshot());
        }
    }

    async fn handle_query(&mut self, query: Query) {
        let key = query.key_expr().as_str().to_string();
        let Some(verb) = keys::parse_user_profile_rpc(&self.site, &key).map(str::to_string) else {
            reply_err(
                &query,
                &WireError::bad_request(format!("no such rpc: {key}")),
            )
            .await;
            return;
        };
        let body: Value = match query.payload() {
            Some(bytes) => match serde_json::from_slice(&bytes.to_bytes()) {
                Ok(value) => value,
                Err(e) => {
                    reply_err(
                        &query,
                        &WireError::bad_request(format!("invalid JSON: {e}")),
                    )
                    .await;
                    return;
                }
            },
            None => Value::Null,
        };
        match self.route(&query, verb.as_str(), &body).await {
            Ok(payload) => reply_json(&query, &payload).await,
            Err(error) => reply_err(&query, &error).await,
        }
    }

    async fn route(
        &mut self,
        _query: &Query,
        verb: &str,
        body: &Value,
    ) -> Result<Value, WireError> {
        match verb {
            "create" => {
                let req: rpc::UserProfileCreate = parse(body)?;
                let id = req.id.unwrap_or_else(Uuid::new_v4);
                let mut new_body = req.body.clone();
                if id == tisvlay_core::DEFAULT_USER_PROFILE_UUID {
                    // Re-creating the reserved default is an upsert with
                    // the name pinned (root ensure_default parity).
                    new_body.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
                } else if self.profiles.contains_key(&id) {
                    return Err(WireError::profile_exists(id));
                }
                let req = rpc::UserProfileCreate {
                    id: Some(id),
                    body: new_body,
                };
                self.profiles.insert(id, req.body.clone());
                self.publish_profile(&id, &req.body).await;
                self.persist();
                self.recompute_idle();
                if self.recompute_remote_armed() {
                    self.publish_remote().await;
                }
                Ok(json!({ "id": id }))
            }
            "update" => {
                let req: rpc::UserProfileUpdate = parse(body)?;
                if !self.profiles.contains_key(&req.id) {
                    return Err(WireError::profile_not_found(req.id));
                }
                if req.id == tisvlay_core::DEFAULT_USER_PROFILE_UUID
                    && req.body.name.trim() != tisvlay_core::DEFAULT_PROFILE_NAME
                {
                    return Err(WireError::bad_request(format!(
                        "default profile's name must be \"{}\"",
                        tisvlay_core::DEFAULT_PROFILE_NAME
                    )));
                }
                self.profiles.insert(req.id, req.body.clone());
                self.publish_profile(&req.id, &req.body).await;
                self.persist();
                self.recompute_idle();
                if self.recompute_remote_armed() {
                    self.publish_remote().await;
                }
                Ok(json!({}))
            }
            "duplicate" => {
                let req: rpc::ProfileDuplicate = parse(body)?;
                let Some(source) = self.profiles.get(&req.id).cloned() else {
                    return Err(WireError::profile_not_found(req.id));
                };
                let id = Uuid::new_v4();
                let mut body = source;
                body.name = req.name;
                self.profiles.insert(id, body.clone());
                self.publish_profile(&id, &body).await;
                self.persist();
                Ok(json!({ "id": id }))
            }
            "delete" => {
                let req: rpc::TvRef = parse(body)?;
                if req.id == tisvlay_core::DEFAULT_USER_PROFILE_UUID {
                    return Err(WireError::profile_reserved(req.id));
                }
                if !self.profiles.contains_key(&req.id) {
                    return Err(WireError::profile_not_found(req.id));
                }
                if self.assignments.values().any(|profile| *profile == req.id) {
                    return Err(WireError::profile_in_use(req.id));
                }
                self.profiles.remove(&req.id);
                self.delete_key(&keys::user_profile(&self.site, &req.id))
                    .await;
                self.persist();
                self.recompute_idle();
                if self.recompute_remote_armed() {
                    self.publish_remote().await;
                }
                Ok(json!({}))
            }
            "info" => Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "state_path": self.state_path,
            })),
            "defaults" => {
                // The hard-coded floor as a Body — the editable default
                // profile sits between this and child profiles.
                let mut floor = UserProfileConfig::full_default();
                floor.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
                serde_json::to_value(&floor)
                    .map_err(|error| WireError::bad_request(format!("encode defaults: {error}")))
            }
            "effective" => {
                let req: rpc::TvRef = parse(body)?;
                Ok(resolved_json(&self.resolve_for(&req.id)))
            }
            "assign" => {
                let req: rpc::UserAssign = parse(body)?;
                if let Some(profile) = req.profile {
                    if !self.profiles.contains_key(&profile) {
                        return Err(WireError::profile_not_found(profile));
                    }
                    self.assignments.insert(req.tv, profile);
                    self.publish_assignment(&req.tv, Some(profile)).await;
                } else {
                    self.assignments.remove(&req.tv);
                    self.delete_key(&keys::user_tv_profile(&self.site, &req.tv))
                        .await;
                }
                self.persist();
                self.recompute_idle();
                if self.recompute_remote_armed() {
                    self.publish_remote().await;
                }
                Ok(json!({}))
            }
            other => Err(WireError::bad_request(format!("no such rpc: {other}"))),
        }
    }

    async fn publish_profile(&mut self, id: &Uuid, body: &UserProfileConfig) {
        match serde_json::to_value(body) {
            Ok(payload) => {
                let key = keys::user_profile(&self.site, id);
                self.publish(&key, &payload).await;
            }
            Err(e) => warn!(%e, "user profile serialize failed"),
        }
    }

    async fn publish_assignment(&mut self, tv: &Uuid, profile: Option<Uuid>) {
        let key = keys::user_tv_profile(&self.site, tv);
        self.publish(&key, &json!({ "profile": profile })).await;
    }

    async fn publish(&mut self, key: &str, payload: &Value) {
        if !self.publishers.contains_key(key) {
            let declared = self
                .user
                .declare_publisher(key.to_string())
                .encoding(Encoding::APPLICATION_JSON)
                .cache(CacheConfig::default().max_samples(1))
                .sample_miss_detection(MissDetectionConfig::default().heartbeat(MISS_HEARTBEAT))
                .publisher_detection()
                .await;
            match declared {
                Ok(publisher) => {
                    self.publishers.insert(key.to_string(), publisher);
                }
                Err(e) => {
                    warn!(%key, %e, "declare publisher failed, sample dropped");
                    return;
                }
            }
        }
        let publisher = self.publishers.get(key).expect("just inserted");
        match serde_json::to_vec(payload) {
            Ok(bytes) => {
                if let Err(e) = publisher.put(bytes).await {
                    warn!(%key, %e, "publish failed");
                }
            }
            Err(e) => warn!(%key, %e, "payload encode failed"),
        }
    }

    /// Delete through the owning publisher so its cache records the
    /// tombstone — late joiners see nothing.
    async fn delete_key(&mut self, key: &str) {
        match self.publishers.remove(key) {
            Some(publisher) => {
                if let Err(e) = publisher.delete().await {
                    warn!(%key, %e, "delete failed");
                }
            }
            None => {
                if let Err(e) = self.user.delete(key).await {
                    warn!(%key, %e, "session delete failed");
                }
            }
        }
    }
}

/// Whether a session-side eager `/serverinfo` pull is worthwhile before
/// this event dispatches. Policy-driven, not an event allow-list: it runs
/// when a live Sunshine source exists, the event is gate-eligible (not an
/// arming edge — those fire at the arming boundary itself), and some armed
/// TV's resolved action is a real dispatch the gate is currently holding
/// down (non-`none`, `while_remote` false) that a freshly-cleared source
/// could release. For the stock floor that is exactly the wake-ish events
/// (unlock, lock-wake, idle-resume) and no others, but a profile that gates
/// any other event is served the same — the event carries no inherent
/// direction, only the resolved action does.
fn eager_refresh_applies(
    event: SessionEvent,
    has_sunshine: bool,
    mut armed_actions: impl Iterator<Item = tisvlay_core::EventAction>,
) -> bool {
    if event.is_remote_edge() || !has_sunshine {
        return false;
    }
    armed_actions.any(|a| a.action() != SessionAction::None && !a.while_remote())
}

/// Pend forever with no drift armed, so the core select stays
/// event-driven between deadlines.
async fn drift_deadline(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The wire projection of a resolved user profile — same field names as
/// the config, every value concrete. Actions render in their serde
/// shape (string, or table when they carry modifiers).
fn resolved_json(resolved: &ResolvedUserProfile) -> Value {
    let action = |a: tisvlay_core::EventAction| serde_json::to_value(a).unwrap_or_default();
    json!({
        "on_session_lock": action(resolved.on_session_lock),
        "on_session_unlock": action(resolved.on_session_unlock),
        "on_lock_wake": action(resolved.on_lock_wake),
        "locked_idle_timeout_seconds": resolved.locked_idle_timeout_seconds,
        "unlocked_idle_timeout_seconds": resolved.unlocked_idle_timeout_seconds,
        "on_idle_timeout": action(resolved.on_idle_timeout),
        "on_idle_resume": action(resolved.on_idle_resume),
        "on_remote_connect": action(resolved.on_remote_connect),
        "on_remote_disconnect": action(resolved.on_remote_disconnect),
        "on_local_return": action(resolved.on_local_return),
        "remote_screencast_arms": resolved.remote_screencast_arms,
    })
}

fn parse<T: DeserializeOwned>(body: &Value) -> Result<T, WireError> {
    serde_json::from_value(body.clone()).map_err(|e| WireError::bad_request(e.to_string()))
}

async fn reply_json(query: &Query, payload: &Value) {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    let key = query.key_expr().clone().into_owned();
    if let Err(e) = query
        .reply(key, bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        warn!(%e, "reply failed");
    }
}

async fn reply_err(query: &Query, error: &WireError) {
    let bytes = serde_json::to_vec(error).unwrap_or_default();
    if let Err(e) = query
        .reply_err(bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        warn!(%e, "reply_err failed");
    }
}

/// Idle timeout for lock-wake detection, in milliseconds.
///
/// Must be short enough that the user goes "idle" in the brief pause
/// between locking the screen (e.g. Super+L) and moving the mouse to wake
/// the panel: `ext-idle-notify-v1` only emits a `Resumed` after an
/// `Idled`, so at 200 ms even a rapid lock-then-move clears the threshold.
const LOCK_WAKE_TIMEOUT_MS: u32 = 200;

/// Notification tag for the always-on lock-wake watch.
const LOCK_WAKE_TAG: u32 = 0;

/// Notification tag for the locked idle watch (registered only when the
/// resolved locked timeout is non-zero).
const LOCKED_IDLE_TAG: u32 = 1;

/// Notification tag for the unlocked (in-session) idle watch (registered
/// only when the resolved unlocked timeout is non-zero).
const UNLOCKED_IDLE_TAG: u32 = 2;

/// Retry period when the wayland watch can't register — the compositor
/// may be restarting; keep trying so idle/lock-wake survive it.
const WATCH_RETRY: Duration = Duration::from_secs(5);

/// Wayland idle adapter: three presence rules on one `ext-idle-notify`
/// connection, routed by tag and gated by lock state.
///
/// - **Lock-wake** (always registered, 200 ms): fires `LockWake` on the
///   first activity of an idle cycle while locked — the lock-screen
///   panel comes back to show the password prompt. While unlocked the
///   same probe is inert, except after a remote disconnect: with
///   `local_return_armed` set, its first `Resumed` fires `LocalReturn`
///   to wake the panel the disconnect blank left dark.
/// - **Locked idle** (the resolved locked timeout): its `Idled` edge
///   while locked fires `IdleTimeout` — a bumped-but-never-unlocked
///   lock screen re-blanks. Its `Resumed` edge is ignored: waking while
///   locked is exclusively the lock-wake rule's job.
/// - **Unlocked idle** (the resolved unlocked timeout): the in-session
///   `Idled`/`Resumed` edges fire `IdleTimeout`/`IdleResume` while
///   unlocked.
///
/// Timeouts re-register on change; registration failures retry (the
/// compositor may be restarting). Runs until the client closes.
pub async fn idle_adapter(
    mut timeouts: watch::Receiver<(u64, u64)>,
    client: SessionClient,
    is_locked: Arc<AtomicBool>,
    local_return_armed: Arc<AtomicBool>,
) {
    loop {
        let (locked_secs, unlocked_secs) = *timeouts.borrow_and_update();
        let mut notifications = vec![tisvlay_core::wayland::IdleNotification {
            tag: LOCK_WAKE_TAG,
            timeout_ms: LOCK_WAKE_TIMEOUT_MS,
        }];
        let mut push_idle = |tag: u32, seconds: u64| {
            if seconds > 0 {
                notifications.push(tisvlay_core::wayland::IdleNotification {
                    tag,
                    timeout_ms: u32::try_from(seconds.saturating_mul(1000)).unwrap_or(u32::MAX),
                });
            }
        };
        push_idle(LOCKED_IDLE_TAG, locked_secs);
        push_idle(UNLOCKED_IDLE_TAG, unlocked_secs);
        let (wl_tx, mut transitions) = mpsc::unbounded_channel();
        let _guard = match start_idle_watch(notifications, wl_tx).await {
            Some(guard) => guard,
            None => {
                warn!("no idle transport available; retrying");
                tokio::select! {
                    changed = timeouts.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    () = tokio::time::sleep(WATCH_RETRY) => {}
                }
                continue;
            }
        };
        let mut lock_wake_sent = false;
        loop {
            tokio::select! {
                transition = transitions.recv() => {
                    let Some((tag, transition)) = transition else { break };
                    let locked = is_locked.load(Ordering::SeqCst);
                    let return_armed = local_return_armed.load(Ordering::SeqCst);
                    if let Some(event) =
                        idle_dispatch(tag, transition, locked, return_armed, &mut lock_wake_sent)
                    {
                        client.send(event);
                    }
                },
                changed = timeouts.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break; // re-register at the new timeouts
                }
            }
        }
    }
}

use tisvlay_core::idle_mutter::IdleWatch;

/// Attach an idle watch over the first transport the running desktop
/// provides: the `ext-idle-notify` Wayland protocol (KWin, wlroots), else
/// GNOME's `org.gnome.Mutter.IdleMonitor`. Both feed the same
/// `(tag, transition)` channel, so the dispatch is transport-agnostic.
/// `None` means neither attached — the caller retries.
async fn start_idle_watch(
    notifications: Vec<tisvlay_core::wayland::IdleNotification>,
    tx: mpsc::UnboundedSender<(u32, tisvlay_core::wayland::IdleTransition)>,
) -> Option<IdleWatch> {
    // Wayland first: it needs no bus and is the native path on the KDE
    // targets. Any failure (protocol absent on Mutter, or no compositor)
    // falls through to the D-Bus IdleMonitor.
    match tisvlay_core::wayland::start_idle_watch(notifications.clone(), tx.clone()) {
        Ok(handle) => return Some(IdleWatch::Wayland(handle)),
        Err(e) => debug!(%e, "wayland idle unavailable; trying Mutter IdleMonitor"),
    }
    match tisvlay_core::idle_mutter::start_idle_watch(notifications, tx).await {
        Ok(handle) => Some(IdleWatch::Mutter(handle)),
        Err(e) => {
            debug!(%e, "Mutter IdleMonitor unavailable");
            None
        }
    }
}

/// Route one tagged idle transition to its session event, honoring the
/// lock state each rule is scoped to. `lock_wake_sent` is the lock-wake
/// latch, threaded through [`lock_wake_policy`].
fn idle_dispatch(
    tag: u32,
    transition: tisvlay_core::wayland::IdleTransition,
    is_locked: bool,
    return_armed: bool,
    lock_wake_sent: &mut bool,
) -> Option<SessionEvent> {
    use tisvlay_core::wayland::IdleTransition;
    match (tag, transition) {
        (LOCK_WAKE_TAG, transition) => {
            let (next_sent, fire) = lock_wake_policy(transition, is_locked, *lock_wake_sent);
            *lock_wake_sent = next_sent;
            if fire {
                debug!("user activity while locked, sending lock wake");
                return Some(SessionEvent::LockWake);
            }
            // While unlocked the same 200 ms probe is the post-remote
            // return watch: the first activity after a disconnect (armed
            // by the core) wakes the panel the disconnect left dark.
            if !is_locked && return_armed && transition == IdleTransition::Resumed {
                debug!("local activity after remote disconnect, sending local return");
                return Some(SessionEvent::LocalReturn);
            }
            None
        }
        (LOCKED_IDLE_TAG, IdleTransition::Idled) if is_locked => Some(SessionEvent::IdleTimeout),
        (UNLOCKED_IDLE_TAG, IdleTransition::Idled) if !is_locked => Some(SessionEvent::IdleTimeout),
        (UNLOCKED_IDLE_TAG, IdleTransition::Resumed) if !is_locked => {
            Some(SessionEvent::IdleResume)
        }
        _ => None,
    }
}

/// Lock-screen wake decision for one idle transition.
///
/// Returns `(next_lock_wake_sent, fire)`. Fires `LockWake` on the first
/// `Resumed` (user activity) of each idle cycle while the screen is
/// locked, at most once per cycle. The next `Idled` clears the latch (a
/// fresh idle cycle begins), so a panel that re-blanks within one lock
/// session wakes again, and a rapid unlock→relock that produced no
/// "unlocked" edge still wakes on the next idle cycle. While unlocked the
/// latch is never set, so no wake fires. Because the latch is per idle
/// cycle (not per lock session), a watch re-registration mid-session
/// can't strand or duplicate a wake.
fn lock_wake_policy(
    transition: tisvlay_core::wayland::IdleTransition,
    is_locked: bool,
    lock_wake_sent: bool,
) -> (bool, bool) {
    match transition {
        // A fresh idle cycle clears the latch so the next resume can
        // wake, independent of any "unlocked" edge. Idle itself never
        // fires a wake.
        tisvlay_core::wayland::IdleTransition::Idled => (false, false),
        tisvlay_core::wayland::IdleTransition::Resumed => {
            let fire = is_locked && !lock_wake_sent;
            // Stay latched (meaningful only while locked) until the next
            // Idled, so a burst of resume events doesn't spam LockWake.
            let next_sent = is_locked && (lock_wake_sent || fire);
            (next_sent, fire)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LOCK_WAKE_TAG, LOCKED_IDLE_TAG, UNLOCKED_IDLE_TAG, eager_refresh_applies, idle_dispatch,
        lock_wake_policy,
    };
    use crate::SessionEvent;
    use tisvlay_core::wayland::IdleTransition;

    #[test]
    fn eager_refresh_fires_on_a_gated_suppressible_dispatch_not_an_event_list() {
        use tisvlay_core::{EventAction, SessionAction};
        // A non-`none` action the gate holds down (a bare token is gated).
        let gated_wake = EventAction::from(SessionAction::ScreenOn);
        // A floor-exempt action that fires while armed — nothing to release.
        let ungated = EventAction::ungated(SessionAction::ScreenOff);
        // No dispatch to release either way.
        let none = EventAction::from(SessionAction::None);

        // Live Sunshine + an armed TV whose action the gate suppresses → pull.
        assert!(eager_refresh_applies(
            SessionEvent::Unlock,
            true,
            [gated_wake].into_iter()
        ));
        // The same policy on any event — no allow-list. A profile that gates
        // an off-going event as a wake is served identically to unlock; the
        // event carries no inherent direction.
        assert!(eager_refresh_applies(
            SessionEvent::Lock,
            true,
            [gated_wake].into_iter()
        ));
        // No live Sunshine source → nothing to refresh.
        assert!(!eager_refresh_applies(
            SessionEvent::Unlock,
            false,
            [gated_wake].into_iter()
        ));
        // Ungated (fires while armed) or `none` (no dispatch) → nothing held.
        assert!(!eager_refresh_applies(
            SessionEvent::Unlock,
            true,
            [ungated].into_iter()
        ));
        assert!(!eager_refresh_applies(
            SessionEvent::Unlock,
            true,
            [none].into_iter()
        ));
        // No armed TV → nothing to un-gate.
        assert!(!eager_refresh_applies(
            SessionEvent::Unlock,
            true,
            std::iter::empty::<EventAction>()
        ));
        // The arming edges never trigger it, whatever the action.
        for edge in [SessionEvent::RemoteConnect, SessionEvent::RemoteDisconnect] {
            assert!(!eager_refresh_applies(edge, true, [gated_wake].into_iter()));
        }
    }

    #[test]
    fn idle_rules_gate_to_their_lock_state() {
        let mut latch = false;
        // Locked idle re-blanks only while locked; its resume edge is
        // silent — waking while locked belongs to lock-wake.
        assert_eq!(
            idle_dispatch(
                LOCKED_IDLE_TAG,
                IdleTransition::Idled,
                true,
                false,
                &mut latch
            ),
            Some(SessionEvent::IdleTimeout)
        );
        assert_eq!(
            idle_dispatch(
                LOCKED_IDLE_TAG,
                IdleTransition::Idled,
                false,
                false,
                &mut latch
            ),
            None
        );
        assert_eq!(
            idle_dispatch(
                LOCKED_IDLE_TAG,
                IdleTransition::Resumed,
                true,
                false,
                &mut latch
            ),
            None
        );
        // Unlocked idle fires both edges, but only while unlocked.
        assert_eq!(
            idle_dispatch(
                UNLOCKED_IDLE_TAG,
                IdleTransition::Idled,
                false,
                false,
                &mut latch
            ),
            Some(SessionEvent::IdleTimeout)
        );
        assert_eq!(
            idle_dispatch(
                UNLOCKED_IDLE_TAG,
                IdleTransition::Resumed,
                false,
                false,
                &mut latch
            ),
            Some(SessionEvent::IdleResume)
        );
        assert_eq!(
            idle_dispatch(
                UNLOCKED_IDLE_TAG,
                IdleTransition::Idled,
                true,
                false,
                &mut latch
            ),
            None
        );
        assert_eq!(
            idle_dispatch(
                UNLOCKED_IDLE_TAG,
                IdleTransition::Resumed,
                true,
                false,
                &mut latch
            ),
            None
        );
        assert!(!latch, "idle rules never touch the lock-wake latch");
    }

    #[test]
    fn lock_wake_tag_threads_the_latch() {
        let mut latch = false;
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Resumed,
                true,
                false,
                &mut latch
            ),
            Some(SessionEvent::LockWake)
        );
        assert!(latch, "first locked resume latches");
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Resumed,
                true,
                false,
                &mut latch
            ),
            None,
            "burst of resumes fires once per idle cycle"
        );
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Idled,
                true,
                false,
                &mut latch
            ),
            None
        );
        assert!(!latch, "a fresh idle cycle clears the latch");
    }

    #[test]
    fn local_return_fires_on_unlocked_resume_when_armed() {
        let mut latch = false;
        // Armed + unlocked: the first resume on the always-on probe is the
        // post-remote return pulse, waking the disconnect-blanked panel.
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Resumed,
                false,
                true,
                &mut latch
            ),
            Some(SessionEvent::LocalReturn)
        );
        // Idle itself is not a return.
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Idled,
                false,
                true,
                &mut latch
            ),
            None
        );
        // Unarmed: the unlocked probe stays inert.
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Resumed,
                false,
                false,
                &mut latch
            ),
            None
        );
        // Locked wins even with the return watch armed — a lock-screen
        // wake, not a return — and still latches lock-wake.
        assert_eq!(
            idle_dispatch(
                LOCK_WAKE_TAG,
                IdleTransition::Resumed,
                true,
                true,
                &mut latch
            ),
            Some(SessionEvent::LockWake)
        );
        assert!(latch, "the locked resume latches lock-wake");
    }

    #[test]
    fn fires_once_per_idle_cycle_on_resume() {
        // Idled while locked: no fire, latch cleared (new idle cycle).
        assert_eq!(
            lock_wake_policy(IdleTransition::Idled, true, false),
            (false, false)
        );
        // First Resumed while locked: fire and latch.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, false),
            (true, true)
        );
        // Second Resumed while still locked (already latched): no fire.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, true),
            (true, false)
        );
    }

    #[test]
    fn idled_clears_latch_so_each_idle_cycle_can_wake() {
        // First resume of a locked idle cycle fires and latches.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, false),
            (true, true)
        );
        // A fresh Idled while still locked clears the latch — even across
        // a rapid unlock/relock that produced no "unlocked" edge.
        assert_eq!(
            lock_wake_policy(IdleTransition::Idled, true, true),
            (false, false)
        );
        // So the next resume fires again.
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, true, false),
            (true, true)
        );
    }

    #[test]
    fn resets_when_unlocked_and_never_fires() {
        assert_eq!(
            lock_wake_policy(IdleTransition::Resumed, false, true),
            (false, false)
        );
        assert_eq!(
            lock_wake_policy(IdleTransition::Idled, false, true),
            (false, false)
        );
    }
}
