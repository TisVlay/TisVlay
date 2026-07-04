//! Daemon library: zenoh session lifecycle (M0), and the full stack —
//! world reactor + zenoh bridge (M2).

mod boot;
mod bridge;
mod discovery;
mod dpms;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tisvlay_core::TvConnector;
use tracing::info;
use tvnext_world::{Command, Reactor, WorldClient};
use uuid::Uuid;
use zenoh::Session;
use zenoh::liveliness::LivelinessToken;

pub use boot::{BootConfig, BootTv, load_boot_config};
pub use discovery::DynDiscovery;
pub use dpms::watch_dpms;

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

/// Open the daemon's zenoh peer session on `socket` and declare liveliness.
pub async fn open(socket: &Path, site: &str) -> Result<Daemon> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket dir {}", parent.display()))?;
    }
    let config = tvnext_wire::bootstrap::peer_config(socket)?;
    let session = zenoh::open(config)
        .await
        .map_err(|e| anyhow!("open zenoh peer session on {}: {e}", socket.display()))?;
    let key = tvnext_wire::keys::daemon_alive(site);
    let alive = session
        .liveliness()
        .declare_token(key.as_str())
        .await
        .map_err(|e| anyhow!("declare liveliness token {key}: {e}"))?;
    info!(site, socket = %socket.display(), "zenoh peer session open, liveliness declared");
    Ok(Daemon { session, alive })
}

/// The full daemon stack: session + world reactor + zenoh bridge.
#[derive(Debug)]
pub struct Stack {
    pub daemon: Daemon,
    pub world: WorldClient,
}

/// Boot the stack on `socket` with the given vendor connectors and
/// discovery backends. The reactor and bridge run as detached tasks for
/// the process lifetime.
pub async fn serve(
    socket: &Path,
    site: &str,
    connectors: Vec<(&str, Arc<dyn TvConnector>)>,
    discoveries: Vec<(&str, Arc<dyn DynDiscovery>)>,
) -> Result<Stack> {
    let daemon = open(socket, site).await?;
    let (reactor, effects, world) = Reactor::new(site, connectors);
    tokio::spawn(reactor.run());
    let discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>> = Arc::new(
        discoveries
            .into_iter()
            .map(|(vendor, discovery)| (vendor.to_string(), discovery))
            .collect(),
    );
    let bridge_session = daemon.session().clone();
    let bridge_site = site.to_string();
    let bridge_world = world.clone();
    tokio::spawn(async move {
        if let Err(e) = bridge::run(
            bridge_session,
            bridge_site,
            effects,
            bridge_world,
            discoveries,
        )
        .await
        {
            tracing::error!(%e, "zenoh bridge exited");
        }
    });
    Ok(Stack { daemon, world })
}

/// DPMS sysfs poll period in production (`watch_dpms` takes it as a
/// parameter so tests can run fast).
const DPMS_PERIOD: Duration = Duration::from_secs(2);

/// Production-shaped entry: serve with the real vendor backends (plus the
/// mock for play), seed TVs from the boot config, optionally watch a DRM
/// connector's DPMS state, run until ctrl-c.
pub async fn run(
    socket: &Path,
    site: &str,
    boot: Option<&Path>,
    dpms_connector: Option<&str>,
    ssdp_port: u16,
) -> Result<()> {
    let (mock_connector, mock_discovery) = tisvlay_mock::new_production_parts();
    let connectors: Vec<(&str, Arc<dyn TvConnector>)> = vec![
        ("mock", Arc::new(mock_connector)),
        ("lg", Arc::new(tisvlay_lg::LgConnector::new())),
        ("cec", Arc::new(tisvlay_cec::CecConnector::new())),
    ];
    let discoveries: Vec<(&str, Arc<dyn DynDiscovery>)> = vec![
        ("mock", Arc::new(mock_discovery)),
        // Pinned SSDP source port: LG TVs reply unicast from ephemeral
        // ports TO this port, so it must match the firewall's allowed UDP
        // port — see docs/firewall.md at the repo root.
        ("lg", Arc::new(tisvlay_lg::LgDiscovery::new(ssdp_port))),
        ("cec", Arc::new(tisvlay_cec::CecDiscovery::new())),
    ];
    let stack = serve(socket, site, connectors, discoveries).await?;
    if let Some(connector) = dpms_connector {
        tokio::spawn(watch_dpms(
            PathBuf::from("/sys/class/drm"),
            connector.to_string(),
            DPMS_PERIOD,
            stack.world.clone(),
        ));
    }
    if let Some(path) = boot {
        for tv in load_boot_config(path)?.tv {
            let id = tv.id.unwrap_or_else(Uuid::new_v4);
            info!(%id, name = %tv.name, vendor = %tv.vendor, "boot TV");
            stack.world.command(Command::AddTv {
                id,
                name: tv.name,
                vendor: tv.vendor,
                config: tv.config,
                managed: tv.managed,
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
