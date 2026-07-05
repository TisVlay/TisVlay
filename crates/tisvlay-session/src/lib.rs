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

mod screensaver;
mod store;

use std::collections::HashMap;
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
        }
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
    /// Resolved user idle timeout (seconds, min enabled across automated
    /// TVs; 0 = disabled) — the wayland idle adapter re-registers on
    /// change.
    pub idle_timeout: watch::Receiver<u64>,
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

    // Liveliness after the rpc queryable: a client that observes the
    // service alive must find the rpc surface already declared.
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

    let (event_tx, events) = mpsc::unbounded_channel();
    let (idle_tx, idle_rx) = watch::channel(0u64);

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
        _alive: alive,
    };
    tokio::spawn(async move {
        core.run(events, queries, meta_sub).await;
        drop(queryable);
    });

    Ok(SessionStack {
        client: SessionClient { tx: event_tx },
        idle_timeout: idle_rx,
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
    idle_tx: watch::Sender<u64>,
    _alive: zenoh::liveliness::LivelinessToken,
}

impl Core {
    async fn run(
        &mut self,
        mut events: mpsc::UnboundedReceiver<SessionEvent>,
        mut queries: mpsc::UnboundedReceiver<Query>,
        meta_sub: zenoh_ext::AdvancedSubscriber<
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
        info!(site = %self.site, "session core running");
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    None => return,
                    Some(event) => self.dispatch(event).await,
                },
                query = queries.recv() => match query {
                    None => return,
                    Some(query) => self.handle_query(query).await,
                },
                sample = meta_sub.recv_async() => match sample {
                    Err(_) => return,
                    Ok(sample) => self.apply_meta(&sample),
                },
            }
        }
    }

    fn apply_meta(&mut self, sample: &zenoh::sample::Sample) {
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
    }

    /// Session event → per-TV user-profile action, called through the
    /// system daemon's rpcs. The tap publish comes first so observers see
    /// the cause before the effects.
    async fn dispatch(&mut self, event: SessionEvent) {
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
    /// profile → hard-coded floor.
    fn action_for(&self, tv: &Uuid, event: SessionEvent) -> SessionAction {
        let resolved = self.resolve_for(tv);
        match event {
            SessionEvent::Lock => resolved.on_session_lock,
            SessionEvent::Unlock => resolved.on_session_unlock,
            SessionEvent::LockWake => resolved.on_lock_wake,
            SessionEvent::IdleTimeout => resolved.on_idle_timeout,
            SessionEvent::IdleResume => resolved.on_idle_resume,
        }
    }

    /// Merge an assignment through the editable default profile down to
    /// the hard-coded floor (root resolve chain).
    fn resolve_for(&self, tv: &Uuid) -> ResolvedUserProfile {
        let assigned = self
            .assignments
            .get(tv)
            .filter(|profile| **profile != tisvlay_core::DEFAULT_USER_PROFILE_UUID)
            .and_then(|profile| self.profiles.get(profile))
            .cloned()
            .unwrap_or_default();
        let default_body = self
            .profiles
            .get(&tisvlay_core::DEFAULT_USER_PROFILE_UUID)
            .cloned()
            .unwrap_or_default();
        assigned.merge_with_base(&default_body).resolve()
    }

    /// Shortest enabled idle timeout across automated TVs' resolved
    /// profiles — the wayland adapter's registration value.
    fn recompute_idle(&self) {
        let mut timeout = 0u64;
        for (tv, meta) in &self.tvs {
            if !meta.automated {
                continue;
            }
            let seconds = self.resolve_for(tv).idle_timeout_seconds;
            if seconds > 0 && (timeout == 0 || seconds < timeout) {
                timeout = seconds;
            }
        }
        self.idle_tx.send_if_modified(|current| {
            let changed = *current != timeout;
            *current = timeout;
            changed
        });
    }

    fn snapshot(&self) -> UserStoreSnapshot {
        UserStoreSnapshot {
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
fn call_tv(system: &Session, site: &str, tv: Uuid, action: SessionAction) {
    let (verb, body) = match action {
        SessionAction::ScreenOff => ("set-screen", json!({ "on": false })),
        SessionAction::ScreenOn => ("set-screen", json!({ "on": true })),
        SessionAction::PowerOff => ("power-off", Value::Null),
        SessionAction::PowerOn => ("power-on", Value::Null),
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
/// the config, every value concrete.
fn resolved_json(resolved: &ResolvedUserProfile) -> Value {
    let action = |a: SessionAction| serde_json::to_value(a).unwrap_or_default();
    json!({
        "on_session_lock": action(resolved.on_session_lock),
        "on_session_unlock": action(resolved.on_session_unlock),
        "on_lock_wake": action(resolved.on_lock_wake),
        "idle_timeout_seconds": resolved.idle_timeout_seconds,
        "on_idle_timeout": action(resolved.on_idle_timeout),
        "on_idle_resume": action(resolved.on_idle_resume),
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

/// Notification tag for the in-session idle watch (registered only when
/// the resolved idle timeout is non-zero).
const IDLE_TAG: u32 = 1;

/// Wayland idle adapter: two presence rules on one `ext-idle-notify`
/// connection, routed by tag. The lock-wake notification (always
/// registered, 200 ms) fires `LockWake` on the first activity of an idle
/// cycle while the screen is locked — the lock-screen panel comes back to
/// show the password prompt. The in-session notification (the resolved
/// idle timeout, re-registering whenever it changes) forwards
/// `IdleTimeout`/`IdleResume` edges. Runs until the client or watch closes.
pub async fn idle_adapter(
    mut timeout: watch::Receiver<u64>,
    client: SessionClient,
    is_locked: Arc<AtomicBool>,
) {
    loop {
        let seconds = *timeout.borrow_and_update();
        let mut notifications = vec![tisvlay_core::wayland::IdleNotification {
            tag: LOCK_WAKE_TAG,
            timeout_ms: LOCK_WAKE_TIMEOUT_MS,
        }];
        if seconds > 0 {
            notifications.push(tisvlay_core::wayland::IdleNotification {
                tag: IDLE_TAG,
                timeout_ms: u32::try_from(seconds.saturating_mul(1000)).unwrap_or(u32::MAX),
            });
        }
        let (wl_tx, mut transitions) = mpsc::unbounded_channel();
        let handle = tisvlay_core::wayland::start_idle_watch(notifications, wl_tx);
        let _guard = match handle {
            Ok(guard) => guard,
            Err(e) => {
                warn!(%e, "wayland idle watch unavailable; waiting for timeout change");
                if timeout.changed().await.is_err() {
                    return;
                }
                continue;
            }
        };
        let mut lock_wake_sent = false;
        loop {
            tokio::select! {
                transition = transitions.recv() => match transition {
                    None => break,
                    Some((LOCK_WAKE_TAG, transition)) => {
                        let (next_sent, fire) = lock_wake_policy(
                            transition,
                            is_locked.load(Ordering::SeqCst),
                            lock_wake_sent,
                        );
                        lock_wake_sent = next_sent;
                        if fire {
                            debug!("user activity while locked, sending lock wake");
                            client.send(SessionEvent::LockWake);
                        }
                    }
                    Some((IDLE_TAG, tisvlay_core::wayland::IdleTransition::Idled)) => {
                        client.send(SessionEvent::IdleTimeout);
                    }
                    Some((IDLE_TAG, tisvlay_core::wayland::IdleTransition::Resumed)) => {
                        client.send(SessionEvent::IdleResume);
                    }
                    Some(_) => {}
                },
                changed = timeout.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break; // re-register at the new timeout
                }
            }
        }
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
    use super::lock_wake_policy;
    use tisvlay_core::wayland::IdleTransition;

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
