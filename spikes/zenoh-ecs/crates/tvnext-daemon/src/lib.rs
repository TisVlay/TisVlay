//! Daemon library: session lifecycle now, ECS world + reactor from M1 on.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use tracing::info;
use zenoh::Session;
use zenoh::liveliness::LivelinessToken;

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

/// Run until ctrl-c, then shut down cleanly.
pub async fn run(socket: &Path, site: &str) -> Result<()> {
    let daemon = open(socket, site).await?;
    tokio::signal::ctrl_c()
        .await
        .context("wait for shutdown signal")?;
    info!("shutting down");
    daemon.close().await
}
