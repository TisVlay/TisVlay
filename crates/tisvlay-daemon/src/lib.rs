//! Daemon library: the zenoh session lifecycle, and the full stack —
//! world reactor + zenoh bridge — under a supervisor.

mod acl;
mod arp;
mod bridge;
mod devices;
mod discovery;
mod dpms;
mod logind;
mod netlink;
mod probe;
mod seat;
mod seed;
pub mod service;
mod store;
pub mod wakeup;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tisvlay_core::TvConnector;
use tisvlay_world::{Command, InEvent, Reactor, SystemEvent, TvRecord, WorldClient};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub use acl::GreeterAccess;
use uuid::Uuid;
use zenoh::Session;
use zenoh::liveliness::LivelinessToken;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

pub use discovery::DynDiscovery;
pub use dpms::watch_dpms;
pub use logind::{run_sleeper, supervise_logind, watch_logind};
pub use seed::{SeedConfig, SeedTv, load_seed_config};
pub use store::load_store;

/// An open daemon session: zenoh peer listening on a Unix socket, with the
/// `tisvlay/<site>/alive/daemon` liveliness token declared.
pub struct Daemon {
    session: Session,
    alive: LivelinessToken,
}

impl fmt::Debug for Daemon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Daemon").finish_non_exhaustive()
    }
}

impl Daemon {
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Undeclare the liveliness token and close the session.
    pub async fn close(self) -> Result<()> {
        self.alive
            .undeclare()
            .await
            .map_err(|e| anyhow!("undeclare liveliness token: {e}"))?;
        self.session
            .close()
            .await
            .map_err(|e| anyhow!("close zenoh session: {e}"))?;
        Ok(())
    }
}

/// Open the daemon's zenoh peer session listening on `socket`.
async fn open_socket(socket: &Path) -> Result<Session> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket dir {}", parent.display()))?;
    }
    let config = tisvlay_protocol::bootstrap::peer_config(socket)?;
    let session = zenoh::open(config)
        .await
        .map_err(|e| anyhow!("open zenoh peer session on {}: {e}", socket.display()))?;
    // The packaged unit runs with UMask=0077, which would leave the
    // socket owner-only; group members are the intended client set
    // (RuntimeDirectory=tisvlay is group-shared the same way).
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))
            .with_context(|| format!("set socket permissions on {}", socket.display()))?;
    }
    Ok(session)
}

/// Declare the daemon's liveliness token. The token is the SDK's
/// availability signal, so it must come last in bring-up: everything
/// the daemon serves is already declared by the time a client can
/// observe it alive (first-rpc-right-after-alive must never race the
/// queryable declarations).
async fn declare_alive(session: &Session, site: &str) -> Result<LivelinessToken> {
    let key = tisvlay_protocol::keys::daemon_alive(site);
    session
        .liveliness()
        .declare_token(key.as_str())
        .await
        .map_err(|e| anyhow!("declare liveliness token {key}: {e}"))
}

/// Open the daemon's zenoh peer session on `socket` and declare liveliness.
pub async fn open(socket: &Path, site: &str) -> Result<Daemon> {
    let session = open_socket(socket).await?;
    let alive = declare_alive(&session, site).await?;
    info!(site, socket = %socket.display(), "zenoh peer session open, liveliness declared");
    Ok(Daemon { session, alive })
}

/// The full daemon stack: session + world reactor + zenoh bridge.
#[must_use = "dropping the stack withdraws the daemon's liveliness token, so clients see it as gone"]
#[derive(Debug)]
pub struct Stack {
    pub daemon: Daemon,
    pub world: WorldClient,
    /// Route-monitor requests from the reactor, awaiting the netlink
    /// watcher (`run` takes them for `spawn_link_monitor`).
    routes: Option<mpsc::UnboundedReceiver<tisvlay_world::RouteRequest>>,
    /// Device-watcher requests from the reactor, when `run` booted the
    /// stack with device watching (`run` takes them for the watcher).
    devices: Option<mpsc::UnboundedReceiver<tisvlay_world::DeviceRequest>>,
    /// The system profile's sleep requests from the reactor (`run` takes
    /// them for the logind sleeper; tests take them with
    /// [`Stack::take_sleep_requests`]).
    sleeps: Option<mpsc::UnboundedReceiver<tisvlay_world::SleepRequest>>,
    /// Shared boot-`Startup` readiness, written by the stream
    /// subscribers; `run` awaits it before the WoL dispatch.
    startup_ready: Arc<StartupReadiness>,
    /// TVs the store held at boot; `run`'s seed config skips them.
    stored: HashSet<Uuid>,
    /// The store writer's reload requests (SIGHUP's path); `None` for an
    /// ephemeral daemon.
    reloads: Option<mpsc::UnboundedSender<store::ReloadRequest>>,
    tasks: Tasks,
}

/// The stack's core tasks: the daemon can't serve without any of them,
/// so [`Stack::run_until`] treats one ending as fatal.
#[derive(Debug)]
struct Tasks {
    reactor: JoinHandle<()>,
    bridge: JoinHandle<Result<()>>,
    /// The store writer; `None` for an ephemeral daemon.
    store: Option<JoinHandle<()>>,
}

/// How long shutdown waits for the store writer to flush its last
/// snapshot.
const STORE_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

impl Stack {
    /// Serve until `shutdown` resolves or a core task ends, then shut down
    /// in order: withdraw liveliness and close the session, stop the world
    /// (which releases the store writer's channel), and wait for the store
    /// to flush its pending snapshot.
    ///
    /// A core task ending is an error naming it. The world, bridge and
    /// store can't be restarted in place, so the process exits and the
    /// service manager restarts it from the store, instead of leaving a
    /// daemon that looks alive but refuses every rpc.
    /// The system profile's sleep requests, once: `run` hands them to the
    /// logind sleeper, and a test can drive [`run_sleeper`] with them.
    pub fn take_sleep_requests(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<tisvlay_world::SleepRequest>> {
        self.sleeps.take()
    }

    pub async fn run_until(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let Self {
            daemon, mut tasks, ..
        } = self;
        let outcome = {
            let store_ended = async {
                match tasks.store.as_mut() {
                    Some(store) => store.await,
                    None => std::future::pending().await,
                }
            };
            // Biased: when the world dies its store channel closes too, and
            // the world is the cause worth naming.
            tokio::select! {
                biased;
                () = shutdown => Ok(()),
                joined = &mut tasks.reactor => Err(match joined {
                    Ok(()) => anyhow!("world reactor stopped"),
                    Err(e) => anyhow::Error::new(e).context("world reactor ended"),
                }),
                joined = &mut tasks.bridge => Err(match joined {
                    Ok(Ok(())) => anyhow!("zenoh bridge stopped"),
                    Ok(Err(e)) => e.context("zenoh bridge failed"),
                    Err(e) => anyhow::Error::new(e).context("zenoh bridge ended"),
                }),
                joined = store_ended => Err(match joined {
                    Ok(()) => anyhow!("store writer stopped"),
                    Err(e) => anyhow::Error::new(e).context("store writer ended"),
                }),
            }
        };
        match &outcome {
            Ok(()) => info!("shutting down"),
            Err(e) => tracing::error!(error = %format!("{e:#}"), "core task ended, shutting down"),
        }
        let closed = daemon.close().await;
        // A handle that already completed in the select must not be polled
        // again; the others are stopped here.
        if !tasks.reactor.is_finished() {
            tasks.reactor.abort();
            // Awaiting the abort is what drops the world, and with it the
            // store writer's sender.
            let _ = tasks.reactor.await;
        }
        if !tasks.bridge.is_finished() {
            tasks.bridge.abort();
        }
        if let Some(store) = tasks.store
            && !store.is_finished()
            && tokio::time::timeout(STORE_FLUSH_TIMEOUT, store)
                .await
                .is_err()
        {
            tracing::warn!("store writer didn't flush within the shutdown timeout");
        }
        outcome.and(closed)
    }
}

/// How long the boot `Startup` wake waits for the armed state to settle
/// before inferring that no session will arm anything. Sized above the
/// resilient-link reconnect window (measured ~855 ms) so a live session's
/// `session_alive` token has propagated by the time absence is inferred;
/// it also subsumes the fleet-replay-burst grace (transport facts ride one
/// batch behind each dial spawn).
const STARTUP_SETTLE: Duration = Duration::from_secs(1);

/// Upper bound on the boot `Startup` wait. Past this the wake fires
/// regardless — the fallback for a present session whose cached mirror the
/// daemon never hydrates (a reconnect that drags past the deadline, or a
/// cache the fresh subscriber cannot reach).
const STARTUP_DEADLINE: Duration = Duration::from_secs(3);

/// Mirror the session's stream fact for one lifetime of its token,
/// forwarding each armed set into the world, which gates the wake-ish
/// system events. The fact is a cache-1 advanced publisher the session
/// serves over its connect-only-peer system link, and the directly-dialed
/// daemon hydrates its last sample: so a daemon starting after (or
/// restarting under) a live session hydrates the standing armed set, and so
/// does a session whose token returns after its link dropped, whose
/// unchanged sample a subscription kept from before would drop as already
/// seen. The returned task ends the subscription when aborted.
async fn mirror_stream(
    session: &Session,
    site: &str,
    world: &WorldClient,
    ready: &Arc<StartupReadiness>,
) -> Result<JoinHandle<()>> {
    let sub = session
        .declare_subscriber(tisvlay_protocol::keys::stream_state(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .recovery(RecoveryConfig::default().heartbeat())
        .await
        .map_err(|e| anyhow!("declare stream subscriber: {e}"))?;
    let world = world.clone();
    let ready = Arc::clone(ready);
    Ok(tokio::spawn(async move {
        while let Ok(sample) = sub.recv_async().await {
            let Ok(state) = serde_json::from_slice::<tisvlay_protocol::payload::StreamState>(
                &sample.payload().to_bytes(),
            ) else {
                continue;
            };
            // The armed set is now known — release the boot gate.
            ready.mirror_seen.store(true, Ordering::Release);
            ready.notify.notify_one();
            if !world.send(InEvent::Stream { armed: state.armed }) {
                return;
            }
        }
    }))
}

/// Shared readiness state for the boot `Startup` gate, written by the
/// stream subscribers and awaited by the startup task.
#[derive(Debug, Default)]
struct StartupReadiness {
    /// A `session_alive` liveliness token is currently observed.
    session_present: AtomicBool,
    /// A stream mirror sample has arrived — hydrated from the
    /// session's `@adv` cache on start, or live — so the armed set is known.
    mirror_seen: AtomicBool,
    /// Woken whenever either flag changes, so the startup task re-evaluates.
    notify: Notify,
}

/// Whether the boot `Startup` wake can fire: the armed state is
/// trustworthy. Pure so the gate logic is unit-testable.
///
/// - `mirror_seen`: a mirror sample landed, so the armed set is known (the
///   daemon-restart-mid-session path).
/// - `settled && !session_present`: the settle window elapsed with no
///   session token in sight, so nothing will arm (the no-session path).
fn startup_ready(mirror_seen: bool, session_present: bool, settled: bool) -> bool {
    mirror_seen || (settled && !session_present)
}

/// Await boot-`Startup` readiness: proceed on a mirror sample, on the
/// settle window elapsing with no session present, or on the fallback
/// deadline.
async fn await_startup_ready(state: Arc<StartupReadiness>) {
    let start = tokio::time::Instant::now();
    let settle_at = start + STARTUP_SETTLE;
    let deadline_at = start + STARTUP_DEADLINE;
    loop {
        let settled = tokio::time::Instant::now() >= settle_at;
        if startup_ready(
            state.mirror_seen.load(Ordering::Acquire),
            state.session_present.load(Ordering::Acquire),
            settled,
        ) {
            return;
        }
        // Before the settle: re-check when it elapses. After it: a session
        // is present but hasn't mirrored, so wait for the mirror (a notify)
        // or the fallback deadline.
        let wake_at = if settled { deadline_at } else { settle_at };
        tokio::select! {
            () = state.notify.notified() => {}
            () = tokio::time::sleep_until(wake_at) => {
                if settled {
                    return; // fallback deadline
                }
            }
        }
    }
}

/// One vendor backend's registration: identity + capabilities for the
/// catalog queryables, the connector, and (optionally) discovery.
pub struct VendorReg {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub connector: Arc<dyn TvConnector>,
    pub discovery: Option<Arc<dyn DynDiscovery>>,
    /// Verify-retry tuning from the backend's `verify_retry_config()`;
    /// `None` = the stock default.
    pub retry: Option<tisvlay_core::RetryConfig>,
}

impl fmt::Debug for VendorReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VendorReg")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl VendorReg {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        connector: Arc<dyn TvConnector>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            capabilities: Vec::new(),
            connector,
            discovery: None,
            retry: None,
        }
    }

    #[must_use]
    pub fn with_discovery(mut self, discovery: Arc<dyn DynDiscovery>) -> Self {
        self.discovery = Some(discovery);
        self
    }

    #[must_use]
    pub fn with_retry(mut self, retry: tisvlay_core::RetryConfig) -> Self {
        self.retry = Some(retry);
        self
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }
}

/// Boot the stack on `socket` with the given vendor registrations,
/// applying the store at `store_path` (if any) before the world starts.
/// The reactor, bridge and store writer run as tasks the returned stack
/// supervises ([`Stack::run_until`]).
pub async fn serve(
    socket: &Path,
    site: &str,
    vendors: Vec<VendorReg>,
    store_path: Option<&Path>,
) -> Result<Stack> {
    serve_stack(socket, site, vendors, store_path, false, None).await
}

/// [`serve`], optionally attaching the device watcher's outlet before the
/// boot dials. With it attached, a local-device TV's dial waits for the
/// watcher's first report, so only an entry point that then spawns the
/// watcher ([`run`]) attaches it; the wire tests serve without one and dial
/// their CEC fakes as before.
async fn serve_stack(
    socket: &Path,
    site: &str,
    vendors: Vec<VendorReg>,
    store_path: Option<&Path>,
    watch_devices: bool,
    sleep: Option<tisvlay_protocol::rpc::SleepSupport>,
) -> Result<Stack> {
    let session = open_socket(socket).await?;
    let connectors: Vec<(&str, Arc<dyn TvConnector>)> = vendors
        .iter()
        .map(|v| (v.id.as_str(), Arc::clone(&v.connector)))
        .collect();
    let (mut reactor, effects, world) = Reactor::new(site, connectors);
    reactor.set_vendor_retry(
        vendors
            .iter()
            .filter_map(|v| v.retry.map(|retry| (v.id.clone(), retry)))
            .collect(),
    );
    let stored_snapshot = store_path.map(load_store).transpose()?;
    // The store writer starts from the snapshot just loaded: its record of
    // what's on disk, which a reload compares the file against.
    let (store, reloads) = match (store_path, &stored_snapshot) {
        (Some(path), Some(base)) => {
            let (persist_tx, persist_rx) = mpsc::unbounded_channel();
            let (reload_tx, reload_rx) = mpsc::unbounded_channel();
            reactor.set_persist(persist_tx);
            let task = tokio::spawn(store::store_task(
                path.to_path_buf(),
                base.clone(),
                persist_rx,
                reload_rx,
                world.clone(),
            ));
            (Some(task), Some(reload_tx))
        }
        _ => (None, None),
    };
    let (routes_tx, routes_rx) = mpsc::unbounded_channel();
    reactor.set_route_monitor(routes_tx);
    let (sleep_tx, sleep_rx) = mpsc::unbounded_channel();
    reactor.set_sleeper(sleep_tx);
    let (wake_tx, wake_rx) = mpsc::unbounded_channel();
    reactor.set_wake_list(wake_tx);
    tokio::spawn(wakeup::write_lists(wakeup::list_path(socket), wake_rx));
    let devices_rx = watch_devices.then(|| {
        let (devices_tx, devices_rx) = mpsc::unbounded_channel();
        reactor.set_device_monitor(devices_tx);
        devices_rx
    });
    // Boot state applies before the reactor runs. The editable default
    // profile comes first, so the catalog always has the inheritance
    // baseline; the store's rows follow and upsert over it, so user edits
    // to the default survive restarts.
    let mut default_profile = tisvlay_core::SystemProfileConfig::full_default();
    default_profile.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
    let mut boot = vec![Command::AddProfile {
        record: Box::new(tisvlay_world::ProfileRecord {
            id: tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
            body: default_profile,
        }),
        reply: None,
    }];
    let mut stored = HashSet::new();
    if let Some(snapshot) = &stored_snapshot {
        for profile in &snapshot.system_profile {
            info!(id = %profile.id, name = %profile.body.name, "store profile");
            boot.push(Command::AddProfile {
                record: Box::new(profile.clone()),
                reply: None,
            });
        }
        for record in &snapshot.tv {
            info!(id = %record.id, name = %record.name, vendor = %record.vendor, "store TV");
            stored.insert(record.id);
            boot.push(Command::AddTv {
                record: Box::new(record.clone()),
                reply: None,
            });
        }
    }
    reactor.seed(stored_snapshot, boot);
    let reactor = tokio::spawn(reactor.run());
    // Production tick source: drives verify timers and dial backoff. Tests
    // can still inject future instants — the world clock is monotonic.
    let ticker = world.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if !ticker.tick(std::time::Instant::now()) {
                return;
            }
        }
    });
    let discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>> = Arc::new(
        vendors
            .iter()
            .filter_map(|v| v.discovery.as_ref().map(|d| (v.id.clone(), Arc::clone(d))))
            .collect(),
    );
    let catalog: Arc<Vec<bridge::VendorMeta>> = Arc::new(
        vendors
            .iter()
            .map(|v| bridge::VendorMeta {
                id: v.id.clone(),
                name: v.name.clone(),
                capabilities: v.capabilities.clone(),
                connector: Arc::clone(&v.connector),
            })
            .collect(),
    );
    let bridge_session = session.clone();
    let bridge_site = site.to_string();
    let bridge_world = world.clone();
    // Introspection served by the bridge (About pane rows).
    let info = tisvlay_protocol::rpc::ServiceInfo {
        version: tisvlay_protocol::VERSION.into(),
        state_path: store_path
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        sleep,
        // Filled per reply from the bridge's live publishers.
        keys: Vec::new(),
    };
    let (bridge_ready, bridge_declared) = tokio::sync::oneshot::channel();
    let bridge = tokio::spawn(bridge::run(
        bridge_session,
        bridge_site,
        effects,
        bridge_world,
        discoveries,
        catalog,
        info,
        reloads.clone(),
        bridge_ready,
    ));
    // Greeter idle transitions arrive as plain puts from `tisvlay-greeter`
    // (or any client standing in for it) — forward them into the world.
    // Declared before the spawn (and thus before the liveliness token
    // below): a greeter that sees the daemon alive must not put into
    // the void.
    let greeter_sub = session
        .declare_subscriber(tisvlay_protocol::keys::greeter_idle(site))
        .await
        .map_err(|e| anyhow!("declare greeter idle subscriber: {e}"))?;
    let greeter_world = world.clone();
    tokio::spawn(async move {
        let sub = greeter_sub;
        while let Ok(sample) = sub.recv_async().await {
            let idle = serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes())
                .ok()
                .and_then(|value| value.get("idle").and_then(serde_json::Value::as_bool));
            let Some(idle) = idle else { continue };
            let event = if idle {
                SystemEvent::GreeterIdle
            } else {
                SystemEvent::GreeterResume
            };
            if !greeter_world.send(InEvent::System {
                event,
                connector: None,
            }) {
                return;
            }
        }
    });
    // Boot-`Startup` readiness: the stream subscribers below write
    // it, and `run` awaits it before the WoL dispatch so the fleet arms (or
    // is known not to) before it wakes.
    let startup_ready = Arc::new(StartupReadiness::default());

    // The session's `session_alive` liveliness token: a crash-robust
    // presence signal the daemon (the directly-dialed peer) observes even
    // though the session's link is a connect-only peer. `history(true)` so
    // a daemon starting after a live session still sees the standing token.
    // The stream mirror follows its lifetime, the way a client mirror
    // follows a service's (docs/architecture.md § Clients): a PUT tells the
    // boot gate to wait for the mirror rather than concluding no session
    // will arm, and subscribes to the stream fact afresh; a DELETE (a
    // crash, a clean exit, or the session's link dropping) ends the
    // subscription and clears the armed set.
    let session_alive_sub = session
        .liveliness()
        .declare_subscriber(tisvlay_protocol::keys::session_alive(site))
        .history(true)
        .await
        .map_err(|e| anyhow!("declare session liveliness subscriber: {e}"))?;
    let session_world = world.clone();
    let session_ready = Arc::clone(&startup_ready);
    let stream_session = session.clone();
    let stream_site = site.to_string();
    tokio::spawn(async move {
        let sub = session_alive_sub;
        let mut mirror: Option<JoinHandle<()>> = None;
        while let Ok(sample) = sub.recv_async().await {
            match sample.kind() {
                zenoh::sample::SampleKind::Put => {
                    session_ready.session_present.store(true, Ordering::Release);
                    session_ready.notify.notify_one();
                    if mirror.is_none() {
                        info!("session present, mirroring its stream state");
                        match mirror_stream(
                            &stream_session,
                            &stream_site,
                            &session_world,
                            &session_ready,
                        )
                        .await
                        {
                            Ok(task) => mirror = Some(task),
                            Err(e) => warn!(error = %e, "couldn't subscribe to the stream fact"),
                        }
                    }
                }
                zenoh::sample::SampleKind::Delete => {
                    info!("session gone, clearing the stream armed set");
                    if let Some(task) = mirror.take() {
                        task.abort();
                        let _ = task.await;
                    }
                    session_ready
                        .session_present
                        .store(false, Ordering::Release);
                    session_ready.notify.notify_one();
                    // Empty armed set disarms every TV via the world's
                    // `apply_stream` — the crash-robust clear.
                    if !session_world.send(InEvent::Stream { armed: Vec::new() }) {
                        return;
                    }
                }
            }
        }
    });
    // Liveliness comes last: a client that observes the daemon alive
    // must find every rpc queryable already declared.
    bridge_declared
        .await
        .map_err(|_| anyhow!("bridge exited before declaring its queryables"))?;
    let alive = declare_alive(&session, site).await?;
    info!(site, socket = %socket.display(), "bridge serving, liveliness declared");
    let daemon = Daemon { session, alive };
    Ok(Stack {
        daemon,
        world,
        routes: Some(routes_rx),
        devices: devices_rx,
        sleeps: Some(sleep_rx),
        startup_ready,
        stored,
        reloads,
        tasks: Tasks {
            reactor,
            bridge,
            store,
        },
    })
}

/// DPMS sysfs poll period in production (`watch_dpms` takes it as a
/// parameter so tests can run fast).
const DPMS_PERIOD: Duration = Duration::from_secs(2);

/// Everything the production entry point needs; `main` builds it from
/// flags.
#[derive(Debug)]
pub struct DaemonOptions {
    pub socket: PathBuf,
    pub site: String,
    /// The daemon's own file — persisted TV table (credentials
    /// included). `None` = ephemeral.
    pub store: Option<PathBuf>,
    /// Declarative seed config re-applied each start; ids already in
    /// the store win.
    pub seed: Option<PathBuf>,
    /// Extra unscoped DPMS watcher (override; probe-matched connectors
    /// are watched automatically).
    pub dpms_connector: Option<String>,
    pub ssdp_port: u16,
    /// Grant the login greeter's group a socket ACL (its dynamic-userdb
    /// account can't be reached via `/etc/group`); disabled by default.
    pub greeter: GreeterAccess,
    /// Grant each seat's active graphical user a socket ACL while their
    /// session is the seat foreground (presence-scoped, `uaccess`-style);
    /// the `tisvlay` group remains the durable grant.
    pub seat_access: bool,
}

/// Production-shaped entry: serve with the real vendor backends (plus the
/// mock for play) from the store, apply the seed config,
/// optionally watch a DRM connector's DPMS state, and run until ctrl-c,
/// SIGTERM, or a core task ending.
pub async fn run(options: DaemonOptions) -> Result<()> {
    // SIGHUP is `systemctl reload`. Its default action kills the process,
    // so the handler goes in before anything else can be signaled.
    let hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .context("install the SIGHUP handler")?;
    use tisvlay_core::TvBackend;
    fn caps(backend: &dyn TvBackend) -> Vec<String> {
        backend
            .capabilities()
            .iter_names()
            .map(|(name, _)| name.to_lowercase())
            .collect()
    }
    let (mock_connector, mock_discovery) = tisvlay_mock::new_production_parts();
    let vendors = vec![
        VendorReg::new("mock", tisvlay_mock::VENDOR_NAME, Arc::new(mock_connector))
            .with_capabilities(caps(&tisvlay_mock::MockBackend))
            .with_retry(tisvlay_mock::MockBackend.verify_retry_config())
            .with_discovery(Arc::new(mock_discovery)),
        VendorReg::new(
            "lg",
            tisvlay_lg::VENDOR_NAME,
            Arc::new(tisvlay_lg::LgConnector::new()),
        )
        .with_capabilities(caps(&tisvlay_lg::LgBackend::new()))
        .with_retry(tisvlay_lg::LgBackend::new().verify_retry_config())
        // Pinned SSDP source port: LG TVs reply unicast from ephemeral
        // ports TO this port, so it must match the firewall's allowed UDP
        // port (why SSDP needs the rule: docs/firewall.md).
        .with_discovery(Arc::new(tisvlay_lg::LgDiscovery::new(options.ssdp_port))),
        VendorReg::new(
            "cec",
            tisvlay_cec::VENDOR_NAME,
            Arc::new(tisvlay_cec::CecConnector::new()),
        )
        .with_capabilities(caps(&tisvlay_cec::CecBackend::new()))
        .with_retry(tisvlay_cec::CecBackend::new().verify_retry_config())
        .with_discovery(Arc::new(tisvlay_cec::CecDiscovery::new())),
    ];
    // The sleep modes logind grants this daemon, for the info reply the GUI
    // reads: bounded, so a slow bus can't hold the boot.
    let sleep = tokio::time::timeout(Duration::from_secs(2), logind::sleep_support())
        .await
        .ok()
        .flatten();
    let mut stack = serve_stack(
        &options.socket,
        &options.site,
        vendors,
        options.store.as_deref(),
        true,
        sleep,
    )
    .await?;
    // The socket is bound and chmod'd 0660 by now; ground the socket ACL
    // here — after the chmod, never before, since a chmod rewrites the ACL
    // mask. The seat set starts empty (the seat watch below fills it in),
    // so this pass grants the greeter group and strips anything stale —
    // including a seat-user entry left by a previous run when seat access
    // is now disabled.
    acl::reconcile(&options.socket, &options.greeter, &Default::default());
    if options.seat_access {
        seat::spawn(options.socket.clone(), options.greeter.clone());
    }
    // The desktop probe auto-spawns a scoped DPMS watcher per
    // EDID-matched connector; the override flag adds one more whose
    // transitions dispatch globally (the escape hatch when the EDID
    // match fails or the target is a mock).
    if let Some(connector) = &options.dpms_connector {
        tokio::spawn(watch_dpms(
            PathBuf::from("/sys/class/drm"),
            connector.clone(),
            false,
            DPMS_PERIOD,
            stack.world.clone(),
        ));
    }
    netlink::spawn_link_monitor(
        stack.world.clone(),
        stack
            .routes
            .take()
            .expect("serve fills the route channel and only run takes it, once"),
    );
    devices::spawn_device_watcher(
        stack.world.clone(),
        stack
            .devices
            .take()
            .expect("run serves with device watching and only it takes the channel, once"),
    );
    tokio::spawn(probe::probe_task(
        stack.daemon.session().clone(),
        options.site.clone(),
        stack.world.clone(),
        PathBuf::from("/sys/class/drm"),
        Duration::from_secs(60),
        DPMS_PERIOD,
    ));
    logind::spawn(stack.world.clone());
    if let Some(requests) = stack.take_sleep_requests() {
        logind::spawn_sleeper(requests, stack.world.clone());
    }

    // The store applied in `serve`; the seed config only adds TVs it
    // didn't hold, so they persist as the real change they are.
    if let Some(path) = &options.seed {
        for tv in load_seed_config(path)?.tv {
            let id = tv.stable_id();
            if stack.stored.contains(&id) {
                continue;
            }
            info!(%id, name = %tv.name, vendor = %tv.vendor, "seed TV");
            stack.world.command(Command::AddTv {
                record: Box::new(TvRecord {
                    id,
                    name: tv.name,
                    vendor: tv.vendor,
                    managed: tv.managed,
                    system_profile: None,
                    primary_input: None,
                    config: tv.config,
                }),
                reply: None,
            });
        }
    }
    // WoL at boot: dispatch `Startup` through the system profiles once the
    // armed state is trustworthy — a mirror sample arrived (hydrated from a
    // live session's cache, or delivered live), or the settle window passed
    // with no session to arm anything, or the fallback deadline elapsed.
    // Awaiting readiness rather than a fixed timer closes the daemon-restart
    // race where a stale-or-absent armed set let a boot wake fire into an
    // empty room under a stream; it also subsumes the fleet-replay grace
    // (transport facts ride one batch behind each dial spawn). The profile's stock
    // `on_startup: power_on` then upgrades the plain boot dial to a wake for
    // every automated, unarmed TV.
    {
        let world = stack.world.clone();
        let ready = Arc::clone(&stack.startup_ready);
        tokio::spawn(async move {
            await_startup_ready(ready).await;
            world.send(InEvent::System {
                event: SystemEvent::Startup,
                connector: None,
            });
        });
    }
    tokio::spawn(reload_on_hangup(hangup, stack.reloads.clone()));
    // SIGTERM is what the service manager sends on stop; without a
    // handler it kills the process before the store flushes.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("install the SIGTERM handler")?;
    let shutdown = async move {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(e) = result {
                    tracing::error!(%e, "ctrl-c handler failed");
                }
            }
            _ = terminate.recv() => {}
        }
    };
    stack.run_until(shutdown).await
}

/// Turn each SIGHUP into a store reload, the same path as the reload rpc.
/// `systemctl reload` reports success as soon as the signal is sent, so
/// the outcome is logged: the world logs an applied edit, and a refusal
/// (with no caller to answer) at warn.
async fn reload_on_hangup(
    mut hangup: tokio::signal::unix::Signal,
    reloads: Option<mpsc::UnboundedSender<store::ReloadRequest>>,
) {
    while hangup.recv().await.is_some() {
        match &reloads {
            Some(reloads) => {
                info!("SIGHUP: reloading the store");
                if reloads.send(store::ReloadRequest { reply: None }).is_err() {
                    return;
                }
            }
            None => tracing::warn!("SIGHUP: this daemon runs without a store, nothing to reload"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SIGHUP takes the reload rpc's path, with no caller to answer.
    /// Nextest runs each test in its own process, so the signal reaches
    /// only this one.
    #[tokio::test]
    async fn sighup_requests_a_store_reload() {
        let hangup =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(reload_on_hangup(hangup, Some(tx)));
        let sent = std::process::Command::new("kill")
            .args(["-HUP", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
        let request = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("SIGHUP requested a reload")
            .expect("the request channel is open");
        assert!(request.reply.is_none());
        task.abort();
    }

    #[test]
    fn startup_ready_truth_table() {
        // A known armed set releases the gate regardless of session/settle.
        assert!(startup_ready(true, true, false));
        assert!(startup_ready(true, false, false));
        // No mirror + a session present: gated even past the settle — the
        // daemon-restart race fix (wait for the reconnected session's
        // mirror, never wake on a stale-or-absent armed set).
        assert!(!startup_ready(false, true, false));
        assert!(!startup_ready(false, true, true));
        // No mirror + no session: gated until the settle, then ready.
        assert!(!startup_ready(false, false, false));
        assert!(startup_ready(false, false, true));
    }

    /// The daemon-restart gate (`#6`): a present session with no mirror yet
    /// holds the wake past the settle window and releases only once its
    /// mirror lands — now hydrated from the session's cache rather than a
    /// re-put. Paused clock so the settle/deadline pass instantly.
    #[tokio::test(start_paused = true)]
    async fn startup_gate_waits_for_a_present_session_to_mirror() {
        let ready = Arc::new(StartupReadiness::default());
        ready.session_present.store(true, Ordering::Release);
        let task = tokio::spawn(await_startup_ready(Arc::clone(&ready)));
        // Past the settle: still gated (session present, no mirror).
        tokio::time::sleep(STARTUP_SETTLE + Duration::from_millis(100)).await;
        assert!(
            !task.is_finished(),
            "gate must hold while a session is present but has not mirrored"
        );
        // The mirror arrives → the gate releases.
        ready.mirror_seen.store(true, Ordering::Release);
        ready.notify.notify_one();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("gate must release once the mirror lands")
            .expect("startup task panicked");
    }

    #[tokio::test(start_paused = true)]
    async fn startup_gate_releases_when_no_session_after_settle() {
        let ready = Arc::new(StartupReadiness::default());
        let start = tokio::time::Instant::now();
        await_startup_ready(Arc::clone(&ready)).await;
        let waited = start.elapsed();
        assert!(
            waited >= STARTUP_SETTLE,
            "must give a live session's token time to appear before inferring absence"
        );
        assert!(
            waited < STARTUP_DEADLINE,
            "no session → release at the settle, not the fallback deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn startup_gate_releases_at_once_on_a_known_armed_set() {
        let ready = Arc::new(StartupReadiness::default());
        ready.mirror_seen.store(true, Ordering::Release);
        let start = tokio::time::Instant::now();
        await_startup_ready(Arc::clone(&ready)).await;
        assert!(
            start.elapsed() < STARTUP_SETTLE,
            "a known armed set releases the gate without waiting"
        );
    }
}
