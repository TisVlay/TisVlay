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
mod logind;
mod monitor;
mod remote;
mod screencast;
mod screensaver;
mod store;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tisvlay_core::{ResolvedUserProfile, SessionAction, UserProfileConfig};
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

pub use logind::spawn as spawn_logind_detector;
pub use monitor::spawn as spawn_portal_monitor;
pub use remote::RemoteReport;
pub use screencast::spawn as spawn_screencast_detector;
pub use screensaver::watch_screensaver;
pub use store::{Assignment, UserProfileRecord, UserStoreSnapshot, load_user_store};

/// Heartbeat period for sample-miss recovery on cached publishers.
const MISS_HEARTBEAT: Duration = Duration::from_millis(500);

/// A session-scoped desktop event, from whichever adapter saw it
/// (ScreenSaver D-Bus, wayland idle, or a test driver).
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
        }
    }

    /// The connect/disconnect edges. They dispatch at the arming boundary
    /// itself, so they bypass the while-remote gate (honoring it would
    /// swallow the very action the edge exists to fire) and stay orthogonal
    /// to the idle-drift window.
    fn is_remote_edge(self) -> bool {
        matches!(self, Self::RemoteConnect | Self::RemoteDisconnect)
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
    /// Shared lock state: the screensaver watcher writes it (seed +
    /// edges), the idle adapter's lock-wake policy and the core's
    /// drift re-assert read it.
    pub is_locked: Arc<AtomicBool>,
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

    // The daemon's liveliness (re)appearance is the mirror re-put cue:
    // the system mirror is a plain put over a connect-only peer, so a
    // daemon restart mid-session must re-learn the current fact without
    // waiting for the next detector edge.
    let daemon_alive_sub = system
        .liveliness()
        .declare_subscriber(keys::daemon_alive(&options.site))
        .history(true)
        .await
        .map_err(|e| anyhow!("subscribe daemon liveliness: {e}"))?;

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

    let (event_tx, events) = mpsc::unbounded_channel();
    let (remote_tx, reports) = mpsc::unbounded_channel();
    let (idle_tx, idle_rx) = watch::channel((0u64, 0u64));
    let is_locked = Arc::new(AtomicBool::new(false));

    let mut core = Core {
        system,
        user,
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
        idle_fired: false,
        screen_on: HashMap::new(),
        drift_at: HashMap::new(),
        remote_sources: remote::RemoteSources::default(),
        remote_armed: HashSet::new(),
        _alive: alive,
    };
    tokio::spawn(async move {
        core.run(
            events,
            reports,
            queries,
            remote_queries,
            meta_sub,
            power_sub,
            daemon_alive_sub,
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
    })
}

struct Core {
    system: Session,
    user: Session,
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
    /// GetActive-seeded auto-login lock, which arrives with no edge).
    is_locked: Arc<AtomicBool>,
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
    _alive: zenoh::liveliness::LivelinessToken,
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
        daemon_alive_sub: zenoh::pubsub::Subscriber<
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
                    Some(event) => self.dispatch(event).await,
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
                sample = daemon_alive_sub.recv_async() => match sample {
                    Err(_) => break 'run,
                    Ok(sample) => {
                        // The token's (re)appearance re-learns the current
                        // fact; its disappearance needs nothing (a re-put
                        // lands when it returns). Recompute first so the
                        // re-put folds in any TVs hydrated by now, then
                        // mirror unconditionally: a put issued while the
                        // link was down was silently dropped, so re-send it
                        // even when the armed set did not move.
                        if sample.kind() == SampleKind::Put {
                            self.recompute_remote_armed();
                            self.mirror_remote().await;
                        }
                    }
                },
                _ = drift_deadline(next_drift) => self.reassert_due(),
            }
        }
        // Clean exit (a channel closed on logout/restart): publish an empty
        // fact to the system mirror so the world disarms. The mirror is a
        // plain put with no daemon-watchable liveliness token — the
        // session's system link is a connect-only peer and cannot serve one
        // per the serve-side law — so departure must actively clear it. A
        // crash that skips this path leaves the last armed set latched until
        // the session returns and re-puts; covering that would need a
        // cross-process heartbeat/TTL that does not exist yet (design
        // pending).
        self.mirror_state(&payload::RemoteSessionState::default())
            .await;
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
            call_tv(&self.system, &self.site, tv, action);
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

    /// Re-put the current fact on the system-socket mirror (the daemon
    /// liveliness re-put path). [`publish_remote`] builds the state once
    /// and mirrors it directly; this rebuilds it for standalone callers.
    async fn mirror_remote(&self) {
        self.mirror_state(&self.remote_state()).await;
    }

    /// Put a fact on the system-socket mirror. A plain put (no cache, no
    /// token) — the session's system link is a connect-only peer, so a
    /// cached pub there would not propagate.
    async fn mirror_state(&self, state: &payload::RemoteSessionState) {
        let key = keys::remote_session(&self.site);
        let bytes = match serde_json::to_vec(state) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(%e, "remote mirror serialize failed");
                return;
            }
        };
        if let Err(e) = self
            .system
            .put(key.as_str(), bytes)
            .encoding(Encoding::APPLICATION_JSON)
            .await
        {
            warn!(%key, %e, "remote mirror put failed");
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

    /// Session event → per-TV user-profile action, called through the
    /// system daemon's rpcs. The tap publish comes first so observers see
    /// the cause before the effects.
    async fn dispatch(&mut self, event: SessionEvent) {
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
        let tap = keys::session_event(&self.site, event.tap_name());
        if let Err(e) = self
            .system
            .put(tap.as_str(), b"{}".to_vec())
            .encoding(Encoding::APPLICATION_JSON)
            .await
        {
            warn!(%tap, %e, "session tap publish failed");
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
            call_tv(&self.system, &self.site, *tv, action);
        }
    }

    /// Root chain per field: assignment → profile → editable default
    /// profile → hard-coded floor. While a TV is armed, an action whose
    /// `while_remote` gate is `false` is suppressed (dispatches nothing)
    /// — a pure gate, so an unarmed TV is never affected and the gate
    /// can never originate an action. The gate is ignored on the
    /// connect/disconnect events (those fire at the arming edge itself,
    /// where honoring the gate would swallow the action).
    fn action_for(&self, tv: &Uuid, event: SessionEvent) -> tisvlay_core::EventAction {
        let resolved = self.resolve_for(tv);
        let action = match event {
            SessionEvent::Lock => resolved.on_session_lock,
            SessionEvent::Unlock => resolved.on_session_unlock,
            SessionEvent::LockWake => resolved.on_lock_wake,
            SessionEvent::IdleTimeout => resolved.on_idle_timeout,
            SessionEvent::IdleResume => resolved.on_idle_resume,
            SessionEvent::RemoteConnect => resolved.on_remote_connect,
            SessionEvent::RemoteDisconnect => resolved.on_remote_disconnect,
        };
        // The gate is inert on the arming edges (dispatch reads only the
        // action for those) and never touches an unarmed TV.
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

/// Fire a TV rpc on the system socket; failures are logged, not fatal —
/// the daemon's own retry tier owns delivery.
/// Pend forever with no drift armed, so the core select stays
/// event-driven between deadlines.
async fn drift_deadline(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn call_tv(system: &Session, site: &str, tv: Uuid, spec: tisvlay_core::EventAction) {
    // The resolved restore flag rides the rpc as resolved intent: the
    // world honors it without knowing the session's event vocabulary,
    // and manual clients (which never set it) stay input-neutral.
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
    let session = system.clone();
    let key = keys::tv_rpc(site, &tv, verb);
    tokio::spawn(async move {
        let replies = match session
            .get(key.as_str())
            .payload(serde_json::to_vec(&body).unwrap_or_default())
            .encoding(Encoding::APPLICATION_JSON)
            .timeout(Duration::from_secs(5))
            .await
        {
            Ok(replies) => replies,
            Err(e) => {
                warn!(%key, %e, "tv rpc failed to send");
                return;
            }
        };
        match replies.recv_async().await {
            Ok(reply) => {
                if let Err(err) = reply.result() {
                    let detail: Value =
                        serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null);
                    warn!(%key, %detail, "tv rpc rejected");
                }
            }
            Err(_) => warn!(%key, "tv rpc got no reply"),
        }
    });
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
///   panel comes back to show the password prompt.
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
        let handle = tisvlay_core::wayland::start_idle_watch(notifications, wl_tx);
        let _guard = match handle {
            Ok(guard) => guard,
            Err(e) => {
                warn!(%e, "wayland idle watch unavailable; retrying");
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
                    if let Some(event) = idle_dispatch(tag, transition, locked, &mut lock_wake_sent) {
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

/// Route one tagged idle transition to its session event, honoring the
/// lock state each rule is scoped to. `lock_wake_sent` is the lock-wake
/// latch, threaded through [`lock_wake_policy`].
fn idle_dispatch(
    tag: u32,
    transition: tisvlay_core::wayland::IdleTransition,
    is_locked: bool,
    lock_wake_sent: &mut bool,
) -> Option<SessionEvent> {
    use tisvlay_core::wayland::IdleTransition;
    match (tag, transition) {
        (LOCK_WAKE_TAG, transition) => {
            let (next_sent, fire) = lock_wake_policy(transition, is_locked, *lock_wake_sent);
            *lock_wake_sent = next_sent;
            fire.then(|| {
                debug!("user activity while locked, sending lock wake");
                SessionEvent::LockWake
            })
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
        LOCK_WAKE_TAG, LOCKED_IDLE_TAG, UNLOCKED_IDLE_TAG, idle_dispatch, lock_wake_policy,
    };
    use crate::SessionEvent;
    use tisvlay_core::wayland::IdleTransition;

    #[test]
    fn idle_rules_gate_to_their_lock_state() {
        let mut latch = false;
        // Locked idle re-blanks only while locked; its resume edge is
        // silent — waking while locked belongs to lock-wake.
        assert_eq!(
            idle_dispatch(LOCKED_IDLE_TAG, IdleTransition::Idled, true, &mut latch),
            Some(SessionEvent::IdleTimeout)
        );
        assert_eq!(
            idle_dispatch(LOCKED_IDLE_TAG, IdleTransition::Idled, false, &mut latch),
            None
        );
        assert_eq!(
            idle_dispatch(LOCKED_IDLE_TAG, IdleTransition::Resumed, true, &mut latch),
            None
        );
        // Unlocked idle fires both edges, but only while unlocked.
        assert_eq!(
            idle_dispatch(UNLOCKED_IDLE_TAG, IdleTransition::Idled, false, &mut latch),
            Some(SessionEvent::IdleTimeout)
        );
        assert_eq!(
            idle_dispatch(
                UNLOCKED_IDLE_TAG,
                IdleTransition::Resumed,
                false,
                &mut latch
            ),
            Some(SessionEvent::IdleResume)
        );
        assert_eq!(
            idle_dispatch(UNLOCKED_IDLE_TAG, IdleTransition::Idled, true, &mut latch),
            None
        );
        assert_eq!(
            idle_dispatch(UNLOCKED_IDLE_TAG, IdleTransition::Resumed, true, &mut latch),
            None
        );
        assert!(!latch, "idle rules never touch the lock-wake latch");
    }

    #[test]
    fn lock_wake_tag_threads_the_latch() {
        let mut latch = false;
        assert_eq!(
            idle_dispatch(LOCK_WAKE_TAG, IdleTransition::Resumed, true, &mut latch),
            Some(SessionEvent::LockWake)
        );
        assert!(latch, "first locked resume latches");
        assert_eq!(
            idle_dispatch(LOCK_WAKE_TAG, IdleTransition::Resumed, true, &mut latch),
            None,
            "burst of resumes fires once per idle cycle"
        );
        assert_eq!(
            idle_dispatch(LOCK_WAKE_TAG, IdleTransition::Idled, true, &mut latch),
            None
        );
        assert!(!latch, "a fresh idle cycle clears the latch");
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
