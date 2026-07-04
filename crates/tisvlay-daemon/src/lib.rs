//! Daemon library: zenoh session lifecycle (M0), and the full stack —
//! world reactor + zenoh bridge (M2).

mod arp;
mod bridge;
mod discovery;
mod dpms;
mod logind;
mod netlink;
mod probe;
mod seed;
mod store;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tisvlay_core::TvConnector;
use tisvlay_world::{Command, InEvent, Reactor, SystemEvent, TvRecord, WorldClient};
use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;
use zenoh::Session;
use zenoh::liveliness::LivelinessToken;

pub use discovery::DynDiscovery;
pub use dpms::watch_dpms;
pub use logind::watch_logind;
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
    let config = tisvlay_wire::bootstrap::peer_config(socket)?;
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
    let key = tisvlay_wire::keys::daemon_alive(site);
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
#[derive(Debug)]
pub struct Stack {
    pub daemon: Daemon,
    pub world: WorldClient,
}

/// One vendor backend's registration: identity + capabilities for the
/// catalog queryables, the connector, and (optionally) discovery.
pub struct VendorReg {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub connector: Arc<dyn TvConnector>,
    pub discovery: Option<Arc<dyn DynDiscovery>>,
}

impl std::fmt::Debug for VendorReg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        }
    }

    pub fn with_discovery(mut self, discovery: Arc<dyn DynDiscovery>) -> Self {
        self.discovery = Some(discovery);
        self
    }

    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }
}

/// Boot the stack on `socket` with the given vendor registrations. The
/// reactor and bridge run as detached tasks for the process lifetime.
pub async fn serve(
    socket: &Path,
    site: &str,
    vendors: Vec<VendorReg>,
    store_path: Option<&Path>,
) -> Result<Stack> {
    let session = open_socket(socket).await?;
    let connectors: Vec<(&str, Arc<dyn TvConnector>)> = vendors
        .iter()
        .map(|v| (v.id.as_str(), Arc::clone(&v.connector)))
        .collect();
    let (mut reactor, effects, world) = Reactor::new(site, connectors);
    if let Some(path) = store_path {
        let (tx, rx) = mpsc::unbounded_channel();
        reactor.set_persist(tx);
        tokio::spawn(store::store_task(path.to_path_buf(), rx));
    }
    tokio::spawn(reactor.run());
    // Seed the editable default profile (root ensure_default parity).
    // AddTv-style replay from the store lands later and upserts over
    // this, so user edits to the default survive restarts.
    let mut default_profile = tisvlay_core::SystemProfileConfig::full_default();
    default_profile.name = tisvlay_core::DEFAULT_PROFILE_NAME.into();
    world.command(Command::AddProfile {
        record: Box::new(tisvlay_world::ProfileRecord {
            id: tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
            body: default_profile,
        }),
        reply: None,
    });
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
    let info = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "state_path": store_path.map(|p| p.display().to_string()).unwrap_or_default(),
    });
    let (bridge_ready, bridge_declared) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Err(e) = bridge::run(
            bridge_session,
            bridge_site,
            effects,
            bridge_world,
            discoveries,
            catalog,
            info,
            bridge_ready,
        )
        .await
        {
            tracing::error!(%e, "zenoh bridge exited");
        }
    });
    // Greeter idle transitions arrive as plain puts from `tisvlay-greeter`
    // (or any client standing in for it) — forward them into the world.
    // Declared before the spawn (and thus before the liveliness token
    // below): a greeter that sees the daemon alive must not put into
    // the void.
    let greeter_sub = session
        .declare_subscriber(tisvlay_wire::keys::greeter_idle(site))
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
    // Liveliness comes last: a client that observes the daemon alive
    // must find every rpc queryable already declared.
    bridge_declared
        .await
        .map_err(|_| anyhow!("bridge exited before declaring its queryables"))?;
    let alive = declare_alive(&session, site).await?;
    info!(site, socket = %socket.display(), "bridge serving, liveliness declared");
    let daemon = Daemon { session, alive };
    Ok(Stack { daemon, world })
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
    pub dpms_connector: Option<String>,
    pub ssdp_port: u16,
}

/// Production-shaped entry: serve with the real vendor backends (plus the
/// mock for play), replay the store, apply the seed config,
/// optionally watch a DRM connector's DPMS state, run until ctrl-c.
pub async fn run(options: DaemonOptions) -> Result<()> {
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
            .with_discovery(Arc::new(mock_discovery)),
        VendorReg::new(
            "lg",
            tisvlay_lg::VENDOR_NAME,
            Arc::new(tisvlay_lg::LgConnector::new()),
        )
        .with_capabilities(caps(&tisvlay_lg::LgBackend))
        // Pinned SSDP source port: LG TVs reply unicast from ephemeral
        // ports TO this port, so it must match the firewall's allowed UDP
        // port — see docs/firewall.md at the repo root.
        .with_discovery(Arc::new(tisvlay_lg::LgDiscovery::new(options.ssdp_port))),
        VendorReg::new(
            "cec",
            tisvlay_cec::VENDOR_NAME,
            Arc::new(tisvlay_cec::CecConnector::new()),
        )
        .with_capabilities(caps(&tisvlay_cec::CecBackend))
        .with_discovery(Arc::new(tisvlay_cec::CecDiscovery::new())),
    ];
    let stack = serve(
        &options.socket,
        &options.site,
        vendors,
        options.store.as_deref(),
    )
    .await?;
    if let Some(connector) = &options.dpms_connector {
        tokio::spawn(watch_dpms(
            PathBuf::from("/sys/class/drm"),
            connector.clone(),
            DPMS_PERIOD,
            stack.world.clone(),
        ));
    }
    netlink::spawn_link_monitor(stack.world.clone());
    tokio::spawn(probe::probe_task(
        stack.daemon.session().clone(),
        options.site.clone(),
        stack.world.clone(),
        PathBuf::from("/sys/class/drm"),
        Duration::from_secs(60),
    ));
    match zbus::Connection::system().await {
        Ok(connection) => {
            let world = stack.world.clone();
            tokio::spawn(async move {
                if let Err(e) = watch_logind(connection, world).await {
                    tracing::warn!(%e, "logind watcher exited");
                }
            });
        }
        Err(e) => tracing::warn!(%e, "no system bus, logind watcher disabled"),
    }

    let mut seeded: HashSet<Uuid> = HashSet::new();
    if let Some(path) = &options.store {
        let snapshot = load_store(path)?;
        for profile in snapshot.system_profile {
            info!(id = %profile.id, name = %profile.body.name, "store profile");
            stack.world.command(Command::AddProfile {
                record: Box::new(profile),
                reply: None,
            });
        }
        for record in snapshot.tv {
            info!(id = %record.id, name = %record.name, vendor = %record.vendor, "store TV");
            seeded.insert(record.id);
            stack.world.command(Command::AddTv {
                record: Box::new(record),
                reply: None,
            });
        }
    }
    if let Some(path) = &options.seed {
        for tv in load_seed_config(path)?.tv {
            let id = tv.stable_id();
            if seeded.contains(&id) {
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
                    automate_when: "always".into(),
                    config: tv.config,
                }),
                reply: None,
            });
        }
    }
    tokio::signal::ctrl_c()
        .await
        .context("wait for shutdown signal")?;
    info!("shutting down");
    stack.daemon.close().await
}
