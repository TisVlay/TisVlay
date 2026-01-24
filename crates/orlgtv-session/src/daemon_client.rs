//! Client for communicating with orlgtvd system daemon.
//!
//! Uses D-Bus to call methods on the `org.orlgtv.Daemon1` interface.

use anyhow::{Context, Result};
use tracing::{debug, warn};
use zbus::proxy;

/// D-Bus service name for orlgtvd.
const DAEMON_SERVICE: &str = "org.orlgtv.Daemon";

/// D-Bus object path for orlgtvd.
const DAEMON_PATH: &str = "/org/orlgtv/Daemon";

/// D-Bus proxy for the orlgtv daemon.
#[proxy(
    interface = "org.orlgtv.Daemon1",
    default_service = "org.orlgtv.Daemon",
    default_path = "/org/orlgtv/Daemon"
)]
trait OrlgtvDaemon {
    /// List configured TVs.
    /// Returns: [(id, name, ip, connected)]
    fn list_tvs(&self) -> zbus::Result<Vec<(String, String, String, bool)>>;

    /// Turn screen on for a TV.
    fn screen_on(&self, tv_id: &str) -> zbus::Result<()>;

    /// Turn screen off for a TV (OLED-safe blank).
    fn screen_off(&self, tv_id: &str) -> zbus::Result<()>;

    /// Power on a TV using Wake-on-LAN.
    fn power_on(&self, tv_id: &str) -> zbus::Result<()>;

    /// Power off a TV.
    fn power_off(&self, tv_id: &str) -> zbus::Result<()>;
}

/// Client for communicating with the orlgtv system daemon.
#[derive(Clone)]
pub struct DaemonClient {
    proxy: OrlgtvDaemonProxy<'static>,
}

impl DaemonClient {
    /// Create a new daemon client connected to the system bus.
    pub async fn new_system() -> Result<Self> {
        let connection = zbus::Connection::system()
            .await
            .context("Failed to connect to system D-Bus")?;

        Self::from_connection(connection).await
    }

    /// Create a new daemon client connected to the session bus (for development).
    pub async fn new_session() -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .context("Failed to connect to session D-Bus")?;

        Self::from_connection(connection).await
    }

    /// Create a client from an existing connection.
    async fn from_connection(connection: zbus::Connection) -> Result<Self> {
        let proxy = OrlgtvDaemonProxy::builder(&connection)
            .destination(DAEMON_SERVICE)?
            .path(DAEMON_PATH)?
            .build()
            .await
            .context("Failed to create D-Bus proxy for orlgtvd")?;

        Ok(Self { proxy })
    }

    /// Check if the daemon is available.
    pub async fn is_available(&self) -> bool {
        // Try to call a simple method to check connectivity
        self.proxy.list_tvs().await.is_ok()
    }

    /// Get list of configured TV IDs.
    pub async fn list_tv_ids(&self) -> Result<Vec<String>> {
        let tvs = self
            .proxy
            .list_tvs()
            .await
            .context("Failed to list TVs from daemon")?;

        Ok(tvs.into_iter().map(|(id, _, _, _)| id).collect())
    }

    /// Turn screen off for a TV (OLED-safe blank).
    pub async fn screen_off(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling ScreenOff on daemon");
        self.proxy
            .screen_off(tv_id)
            .await
            .with_context(|| format!("Failed to turn screen off for TV: {}", tv_id))
    }

    /// Turn screen on for a TV.
    pub async fn screen_on(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling ScreenOn on daemon");
        self.proxy
            .screen_on(tv_id)
            .await
            .with_context(|| format!("Failed to turn screen on for TV: {}", tv_id))
    }

    /// Power off a TV.
    pub async fn power_off(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling PowerOff on daemon");
        self.proxy
            .power_off(tv_id)
            .await
            .with_context(|| format!("Failed to power off TV: {}", tv_id))
    }

    /// Power on a TV using Wake-on-LAN.
    pub async fn power_on(&self, tv_id: &str) -> Result<()> {
        debug!(tv_id = %tv_id, "Calling PowerOn on daemon");
        self.proxy
            .power_on(tv_id)
            .await
            .with_context(|| format!("Failed to power on TV: {}", tv_id))
    }

    /// Execute an action on all configured TVs.
    ///
    /// Returns the number of successful operations.
    pub async fn execute_on_all_tvs<F, Fut>(&self, action_name: &str, action: F) -> usize
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let tv_ids = match self.list_tv_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                warn!("Failed to list TVs: {}", e);
                return 0;
            }
        };

        if tv_ids.is_empty() {
            debug!("No TVs configured, skipping {}", action_name);
            return 0;
        }

        let mut success_count = 0;
        for tv_id in tv_ids {
            match action(tv_id.clone()).await {
                Ok(()) => {
                    debug!(tv_id = %tv_id, action = %action_name, "Action succeeded");
                    success_count += 1;
                }
                Err(e) => {
                    warn!(tv_id = %tv_id, action = %action_name, error = %e, "Action failed");
                }
            }
        }

        success_count
    }
}
