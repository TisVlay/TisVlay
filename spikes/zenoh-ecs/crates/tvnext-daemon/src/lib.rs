//! Daemon library: zenoh session lifecycle (M0), and the full stack —
//! world reactor + zenoh bridge (M2).

mod boot;
mod bridge;

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use tisvlay_core::TvConnector;
use tracing::info;
use tvnext_world::{Command, Reactor, WorldClient};
use uuid::Uuid;
use zenoh::Session;
use zenoh::liveliness::LivelinessToken;

pub use boot::{BootConfig, BootTv, load_boot_config};

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

/// Boot the stack on `socket` with the given vendor connectors. The
/// reactor and bridge run as detached tasks for the process lifetime.
pub async fn serve(
    socket: &Path,
    site: &str,
    connectors: Vec<(&str, Arc<dyn TvConnector>)>,
) -> Result<Stack> {
    let daemon = open(socket, site).await?;
    let (reactor, effects, world) = Reactor::new(site, connectors);
    tokio::spawn(reactor.run());
    let bridge_session = daemon.session().clone();
    let bridge_site = site.to_string();
    let bridge_world = world.clone();
    tokio::spawn(async move {
        if let Err(e) = bridge::run(bridge_session, bridge_site, effects, bridge_world).await {
            tracing::error!(%e, "zenoh bridge exited");
        }
    });
    Ok(Stack { daemon, world })
}

/// Production-shaped entry: serve with the mock backend registered, seed
/// TVs from the boot config, run until ctrl-c.
pub async fn run(socket: &Path, site: &str, boot: Option<&Path>) -> Result<()> {
    let (connector, _discovery) = tisvlay_mock::new_production_parts();
    let stack = serve(socket, site, vec![("mock", Arc::new(connector))]).await?;
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
