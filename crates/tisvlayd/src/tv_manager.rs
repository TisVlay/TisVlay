//! TV connection manager.
//!
//! Manages connections to multiple TVs using vendor-agnostic trait objects.
//!
//! Uses per-TV locking so that operations on different TVs can proceed
//! concurrently. The outer `RwLock<HashMap>` is held only briefly for
//! lookup; network I/O runs under a per-TV `tokio::sync::Mutex`.

use crate::registry::BackendRegistry;
use indexmap::IndexMap;
use std::path::PathBuf;
use std::sync::Arc;
use tisvlay_core::{
    Capabilities, PowerState, ResolvedSystemProfile, SessionAction, SystemConfig, SystemSection,
    TvConfig, TvConnection, TvState, UserProfileConfig, UserSection,
};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

/// Error type for TV manager operations.
#[derive(Debug, thiserror::Error)]
pub enum TvManagerError {
    /// TV not found in the managed set.
    #[error("TV not found: {0}")]
    TvNotFound(String),

    /// TV exists but is not currently connected.
    #[error("Not connected to TV: {0}")]
    NotConnected(String),

    /// TV is intentionally powered off (no reconnection attempts).
    #[error("TV {0} is powered off")]
    PoweredOff(String),

    /// Wake-on-LAN failed for a TV.
    #[error("Wake failed for TV {tv_id}: {source}")]
    WakeFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// Connection to a TV failed.
    #[error("Connection failed for TV {tv_id}: {source}")]
    ConnectionFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// A command executed on a TV connection failed.
    #[error("Command failed on TV {tv_id}: {source}")]
    CommandFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// Wake not supported for this TV.
    #[error("Wake not supported for TV: {0}")]
    WakeNotSupported(String),

    /// Pairing failed.
    #[error("Pairing failed: {0}")]
    PairingFailed(Box<dyn std::error::Error + Send + Sync>),

    /// Configuration I/O error (load/save).
    #[error("Config error: {0}")]
    ConfigError(String),
}

/// Event emitted when TV state changes.
#[derive(Debug, Clone)]
pub struct StateChangeEvent {
    /// The TV identifier.
    pub tv_id: String,
    /// The new state (JSON-encoded).
    pub state_json: String,
}

impl StateChangeEvent {
    /// Create a state change event for a connection status change.
    pub fn connection_change(tv_id: &str, connected: bool, error: Option<&str>) -> Self {
        let state = serde_json::json!({
            "connected": connected,
            "error": error,
        });
        Self {
            tv_id: tv_id.to_string(),
            state_json: state.to_string(),
        }
    }
}

/// Basic TV information for D-Bus properties.
#[derive(Debug, Clone)]
pub struct TvInfo {
    pub name: String,
    /// Display address (extracted from vendor-specific config via connector).
    pub address: String,
    pub vendor: String,
    pub managed: bool,
}

/// Managed TV connection state.
struct ManagedTv {
    /// TV configuration.
    config: TvConfig,
    /// Active connection (None if disconnected).
    connection: Option<Box<dyn TvConnection>>,
    /// TV was intentionally powered off -- don't try to reconnect until power_on.
    powered_off: bool,
    /// Resolved system profile for daemon lifecycle events.
    ///
    /// Resolved from the TV's `profile.system` assignment against the
    /// system profiles defined in the config. Updated on config reload
    /// and when the profile assignment changes via `set_tv_option`.
    system_profile: ResolvedSystemProfile,
}

impl ManagedTv {
    fn new(config: TvConfig, system_profile: ResolvedSystemProfile) -> Self {
        Self {
            config,
            connection: None,
            powered_off: false,
            system_profile,
        }
    }

    fn is_connected(&self) -> bool {
        self.connection
            .as_ref()
            .map(|c| c.is_connected())
            .unwrap_or(false)
    }

    fn capabilities(&self) -> Capabilities {
        self.connection
            .as_ref()
            .map(|c| c.capabilities())
            .unwrap_or(Capabilities::empty())
    }
}

/// Map value for a single TV in `TvManager::tvs`.
///
/// The `bool` is the `managed` flag stored outside the per-TV mutex so that
/// `list_managed_tv_ids` can scan it with only the outer read lock.
type TvEntry = (bool, Arc<Mutex<ManagedTv>>);

/// TV connection manager.
///
/// Thread-safe manager for multiple TV connections.
/// Uses per-TV locking so operations on different TVs proceed concurrently.
/// Connection resilience (auto-reconnect, WoL retry) is handled by the backends.
pub struct TvManager {
    /// Path to the configuration file for load/save operations.
    config_path: PathBuf,
    /// Backend registry for vendor support.
    registry: Arc<BackendRegistry>,
    /// Managed TVs by ID -- each TV has its own mutex for independent locking.
    /// The outer RwLock protects the IndexMap itself (add/remove); individual TV
    /// operations hold only the per-TV Mutex. IndexMap preserves insertion order
    /// so list_tvs returns a stable, deterministic list matching the config file order.
    ///
    /// The `bool` is the `managed` flag, stored outside the per-TV mutex so that
    /// `list_managed_tv_ids` can scan it with only the outer read lock — no per-TV
    /// lock acquisition, no risk of blocking behind a slow `power_on`.
    tvs: Arc<RwLock<IndexMap<String, TvEntry>>>,
    /// System-level config section (system profiles for daemon lifecycle events).
    system_section: RwLock<SystemSection>,
    /// User-level config section (user profiles for session event defaults).
    user_section: RwLock<UserSection>,
    /// Serializes all load-modify-save disk operations (save_tv_config, delete_tv_config,
    /// set_tv_option). Without this, two concurrent callers can each load a stale snapshot
    /// and one overwrites the other's change on disk.
    disk_write_lock: Mutex<()>,
    /// Channel to send state change events.
    state_change_tx: mpsc::UnboundedSender<StateChangeEvent>,
}

/// System lifecycle event type.
///
/// Used by `logind` and the daemon startup path to dispatch per-TV
/// actions from each TV's resolved system profile.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SystemEvent {
    Startup,
    Shutdown,
    Reboot,
    NoSession,
}

/// Apply a key-value TV option to a `TvConfig`.
///
/// Used by `set_tv_option` to update both the disk config and the in-memory
/// config with a single shared field-mapping, so neither can drift from the other.
fn apply_tv_option(tv: &mut TvConfig, key: &str, value: &str) -> Result<(), TvManagerError> {
    match key {
        "hdmi_input" => {
            tv.hdmi_input = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "only_when_active" => {
            tv.only_when_active = value
                .parse()
                .map_err(|_| TvManagerError::ConfigError("Invalid bool".into()))?;
        }
        "managed" => {
            tv.managed = value
                .parse()
                .map_err(|_| TvManagerError::ConfigError("Invalid bool".into()))?;
        }
        "name" => {
            tv.name = value.to_string();
        }
        "profile.system" => {
            tv.profile.system = value.to_string();
        }
        _ => {
            return Err(TvManagerError::ConfigError(format!(
                "Unknown TV option: {}",
                key
            )));
        }
    }
    Ok(())
}

impl TvManager {
    /// Create a new TV manager from configuration.
    ///
    /// Returns the manager and a receiver for state change events.
    pub fn new(
        config: SystemConfig,
        config_path: PathBuf,
        registry: Arc<BackendRegistry>,
    ) -> (Self, mpsc::UnboundedReceiver<StateChangeEvent>) {
        // Use IndexMap to preserve config file insertion order so GetTvs
        // returns a stable, deterministic list across restarts.
        let mut tvs = IndexMap::new();
        for tv_config in &config.tv {
            let system_profile = config.resolve_system_profile(tv_config);
            tvs.insert(
                tv_config.id.clone(),
                (
                    tv_config.managed,
                    Arc::new(Mutex::new(ManagedTv::new(
                        tv_config.clone(),
                        system_profile,
                    ))),
                ),
            );
        }

        let tvs = Arc::new(RwLock::new(tvs));
        let (state_change_tx, state_change_rx) = mpsc::unbounded_channel();
        let system_section = RwLock::new(config.system.clone());
        let user_section = RwLock::new(config.user.clone());

        let manager = Self {
            config_path,
            registry,
            tvs,
            system_section,
            user_section,
            disk_write_lock: Mutex::new(()),
            state_change_tx,
        };

        (manager, state_change_rx)
    }

    /// Emit a state change event.
    fn emit_state_change(&self, event: StateChangeEvent) {
        if let Err(e) = self.state_change_tx.send(event) {
            debug!("Failed to send state change event: {}", e);
        }
    }

    /// Get a per-TV Arc, briefly holding the outer read lock for lookup only.
    async fn get_tv(&self, tv_id: &str) -> Result<Arc<Mutex<ManagedTv>>, TvManagerError> {
        self.tvs
            .read()
            .await
            .get(tv_id)
            .map(|(_, arc)| arc.clone())
            .ok_or_else(|| TvManagerError::TvNotFound(tv_id.to_string()))
    }

    /// Get the system action for a specific TV and event.
    ///
    /// Reads the TV's resolved system profile to determine the action.
    /// Returns `SessionAction::None` if the TV is not found.
    async fn get_system_action(&self, tv_id: &str, event: SystemEvent) -> SessionAction {
        let tv_arc = match self.get_tv(tv_id).await {
            Ok(arc) => arc,
            Err(_) => return SessionAction::None,
        };
        let tv = tv_arc.lock().await;
        match event {
            SystemEvent::Startup => tv.system_profile.on_startup,
            SystemEvent::Shutdown => tv.system_profile.on_shutdown,
            SystemEvent::Reboot => tv.system_profile.on_reboot,
            SystemEvent::NoSession => tv.system_profile.on_no_session,
        }
    }

    /// Execute a system event on all managed TVs using each TV's resolved system profile.
    pub async fn execute_system_event(&self, event: SystemEvent, context: &str) {
        let tv_ids = self.list_managed_tv_ids().await;
        for tv_id in &tv_ids {
            let action = self.get_system_action(tv_id, event).await;
            if action == SessionAction::None {
                continue;
            }

            let result: Result<(), TvManagerError> = match action {
                SessionAction::ScreenOff => self.screen_off(tv_id).await,
                SessionAction::ScreenOn => self.screen_on(tv_id).await,
                SessionAction::PowerOff => self.power_off(tv_id).await,
                SessionAction::PowerOn => self.power_on(tv_id).await,
                SessionAction::None => Ok(()),
            };

            match result {
                Ok(()) => {
                    debug!(tv_id = %tv_id, action = ?action, context = context, "Action executed");
                }
                Err(e) => {
                    warn!(tv_id = %tv_id, action = ?action, context = context, error = %e,
                        "Failed to execute action");
                }
            }
        }
    }

    /// Execute the configured `on_startup` action for all managed TVs,
    /// but only on system boot — not on daemon restart.
    ///
    /// Uses a marker file in `/run/tisvlay/` (tmpfs, cleared on every boot)
    /// to distinguish system boot from service restart. This prevents
    /// unexpected TV power-on during unrelated `systemctl restart`.
    ///
    /// Runs in the background so D-Bus registration is not blocked.
    /// The daemon is reachable immediately; TVs come online when ready.
    ///
    /// Each managed TV is handled according to its own resolved system profile:
    /// - `PowerOn`: WoL + connect + screen on + HDMI input switch
    /// - Other actions: dispatched directly
    /// - `None`: skipped
    pub fn on_startup(self: &Arc<Self>) {
        const BOOT_MARKER: &str = "/run/tisvlay/booted";

        let marker_path = std::path::Path::new(BOOT_MARKER);
        if marker_path.exists() {
            info!("Skipping on_startup (service restart, not system boot)");
            return;
        }

        // Create the marker for subsequent restarts.
        // Failure is non-fatal but means on_startup will fire on every restart.
        if let Some(dir) = marker_path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                warn!(error = %e, "Failed to create boot marker directory — on_startup will fire on every restart");
            }
        }
        if let Err(e) = std::fs::write(marker_path, "") {
            warn!(error = %e, "Failed to write boot marker — on_startup will fire on every restart");
        }

        let manager = self.clone();
        tokio::spawn(async move {
            info!("Executing on_startup per-TV actions");

            // Collect per-TV startup actions and configs for TVs that need WoL.
            let tv_ids = manager.list_managed_tv_ids().await;

            // First pass: handle PowerOn TVs (WoL + connect flow).
            for tv_id in &tv_ids {
                let action = manager.get_system_action(tv_id, SystemEvent::Startup).await;
                if action != SessionAction::PowerOn {
                    continue;
                }

                // Read config under per-TV lock.
                let tv_config = {
                    let tv_arc = match manager.get_tv(tv_id).await {
                        Ok(arc) => arc,
                        Err(_) => continue,
                    };
                    let tv = tv_arc.lock().await;
                    if !manager.registry.supports_wake(&tv.config) {
                        warn!(tv_id = %tv_id, "Startup PowerOn: TV does not support wake, skipping");
                        continue;
                    }
                    tv.config.clone()
                };

                info!(tv_id = %tv_id, "Waking TV and waiting for connection...");
                let connection = match manager.registry.wake_and_connect_tv(&tv_config).await {
                    Ok(conn) => {
                        info!(tv_id = %tv_id, "Startup: TV is online");
                        conn
                    }
                    Err(e) => {
                        warn!(tv_id = %tv_id, error = %e, "Startup: TV did not come online after wake");
                        continue;
                    }
                };

                // Store the connection so subsequent commands don't reconnect.
                {
                    let tvs = manager.tvs.read().await;
                    if let Some((_, tv_arc)) = tvs.get(tv_id.as_str()) {
                        let mut tv = tv_arc.lock().await;
                        tv.connection = Some(connection);
                        tv.powered_off = false;
                    }
                }
                manager.emit_state_change(StateChangeEvent::connection_change(tv_id, true, None));

                match manager.screen_on(tv_id).await {
                    Ok(()) => info!(tv_id = %tv_id, "Startup: screen on"),
                    Err(e) => warn!(tv_id = %tv_id, error = %e, "Startup: failed to turn screen on"),
                }

                if let Some(ref input) = tv_config.hdmi_input {
                    match manager.switch_input(tv_id, input).await {
                        Ok(()) => info!(tv_id = %tv_id, input = %input, "Startup: switched input"),
                        Err(e) => warn!(tv_id = %tv_id, error = %e, "Startup: failed to switch input"),
                    }
                }
            }

            // Second pass: handle non-PowerOn actions (ScreenOn, ScreenOff, PowerOff).
            for tv_id in &tv_ids {
                let action = manager.get_system_action(tv_id, SystemEvent::Startup).await;
                match action {
                    SessionAction::None | SessionAction::PowerOn => continue, // already handled or skip
                    _ => {}
                }

                let result: Result<(), TvManagerError> = match action {
                    SessionAction::ScreenOff => manager.screen_off(tv_id).await,
                    SessionAction::ScreenOn => manager.screen_on(tv_id).await,
                    SessionAction::PowerOff => manager.power_off(tv_id).await,
                    _ => Ok(()),
                };

                match result {
                    Ok(()) => {
                        debug!(tv_id = %tv_id, action = ?action, "Startup action executed");
                    }
                    Err(e) => {
                        warn!(tv_id = %tv_id, action = ?action, error = %e,
                            "Startup: failed to execute action");
                    }
                }
            }
        });
    }

    /// Get the admin baseline for user profiles.
    ///
    /// Returns the `[user.profiles.default]` from the system config,
    /// or an empty UserProfileConfig if not defined.
    pub async fn get_user_profile_default(&self) -> UserProfileConfig {
        let user = self.user_section.read().await;
        user.profiles.get("default").cloned().unwrap_or_default()
    }

    /// Get all user profiles as (name, UserProfileConfig) pairs.
    pub async fn list_user_profiles(&self) -> Vec<(String, UserProfileConfig)> {
        let user = self.user_section.read().await;
        user.profiles
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get all system profiles as (name, SystemProfileConfig) pairs.
    pub async fn list_system_profiles(&self) -> Vec<(String, tisvlay_core::SystemProfileConfig)> {
        let system = self.system_section.read().await;
        system
            .profiles
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get the system profile name assigned to a TV.
    pub async fn get_tv_system_profile(&self, tv_id: &str) -> Option<String> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.config.profile.system.clone())
    }

    /// List all configured TV IDs.
    pub async fn list_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        tvs.keys().cloned().collect()
    }

    /// Get basic TV information.
    pub async fn get_tv_info(&self, tv_id: &str) -> Option<TvInfo> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        let address = self.registry.display_address(&tv.config);
        Some(TvInfo {
            name: tv.config.name.clone(),
            address,
            vendor: tv.config.vendor.clone(),
            managed: tv.config.managed,
        })
    }

    /// List IDs of TVs that participate in automated management.
    pub async fn list_managed_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        tvs.iter()
            .filter_map(|(id, (managed, _))| if *managed { Some(id.clone()) } else { None })
            .collect()
    }

    /// Check if a TV is connected.
    pub async fn is_connected(&self, tv_id: &str) -> bool {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => {
                let tv = tv_arc.lock().await;
                tv.is_connected()
            }
            Err(_) => false,
        }
    }

    /// Get TV capabilities.
    pub async fn get_capabilities(&self, tv_id: &str) -> Option<Capabilities> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.capabilities())
    }

    /// Force reconnection to a TV.
    ///
    /// Disconnects (if connected), then reconnects under a single per-TV lock.
    pub async fn reconnect(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;
        let mut tv = tv_arc.lock().await;

        // Disconnect if connected
        if let Some(mut conn) = tv.connection.take() {
            let _ = conn.close().await;
        }

        if tv.powered_off {
            return Err(TvManagerError::PoweredOff(tv_id.to_string()));
        }

        let display_addr = self.registry.display_address(&tv.config);
        info!(tv_id = %tv_id, address = %display_addr, vendor = %tv.config.vendor, "Reconnecting to TV");

        let connection = self.registry.connect_tv(&tv.config).await.map_err(|e| {
            TvManagerError::ConnectionFailed {
                tv_id: tv_id.to_string(),
                source: e,
            }
        })?;

        info!(tv_id = %tv_id, "Connected to TV");
        tv.connection = Some(connection);

        drop(tv);
        self.emit_state_change(StateChangeEvent::connection_change(tv_id, true, None));

        Ok(())
    }

    /// Execute a command on a TV connection.
    ///
    /// Connects if not already connected, then executes the command.
    /// Only holds the per-TV mutex -- other TVs are not blocked.
    /// Connection resilience (stale connection retry) is handled by
    /// the backend, not here.
    async fn with_connection<F, T>(&self, tv_id: &str, f: F) -> Result<T, TvManagerError>
    where
        F: FnOnce(
            &mut dyn TvConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>,
        >,
    {
        let tv_arc = self.get_tv(tv_id).await?;
        let mut tv = tv_arc.lock().await;

        // Connect if needed (under the same lock to prevent TOCTOU)
        if !tv.is_connected() {
            if tv.powered_off {
                return Err(TvManagerError::PoweredOff(tv_id.to_string()));
            }

            let display_addr = self.registry.display_address(&tv.config);
            info!(tv_id = %tv_id, address = %display_addr, vendor = %tv.config.vendor, "Connecting to TV");

            let connection = self.registry.connect_tv(&tv.config).await.map_err(|e| {
                TvManagerError::ConnectionFailed {
                    tv_id: tv_id.to_string(),
                    source: e,
                }
            })?;

            info!(tv_id = %tv_id, "Connected to TV");
            tv.connection = Some(connection);
            self.emit_state_change(StateChangeEvent::connection_change(tv_id, true, None));
        }

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| TvManagerError::NotConnected(tv_id.to_string()))?;

        f(connection.as_mut()).await.map_err(|e| {
            warn!(tv_id = %tv_id, error = %e, "Command failed");
            TvManagerError::CommandFailed {
                tv_id: tv_id.to_string(),
                source: e,
            }
        })
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV using Wake-on-LAN.
    ///
    /// The backend owns all transport-specific retry logic (WoL resend,
    /// boot wait, connect retry) via `wake_and_connect()`.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;

        // Read config and clear state under per-TV lock
        let config = {
            let mut tv = tv_arc.lock().await;

            if !self.registry.supports_wake(&tv.config) {
                return Err(TvManagerError::WakeNotSupported(tv_id.to_string()));
            }

            let display_addr = self.registry.display_address(&tv.config);
            info!(tv_id = %tv_id, address = %display_addr, "Waking TV");

            // Drop any stale connection and clear the powered-off flag
            tv.powered_off = false;
            if let Some(mut conn) = tv.connection.take() {
                let _ = conn.close().await;
            }

            tv.config.clone()
        };

        // Backend handles wake + boot wait + connect retry (no lock held)
        let connection = self
            .registry
            .wake_and_connect_tv(&config)
            .await
            .map_err(|e| TvManagerError::WakeFailed {
                tv_id: tv_id.to_string(),
                source: e,
            })?;

        // Store the connection under per-TV lock
        {
            let mut tv = tv_arc.lock().await;
            tv.connection = Some(connection);
        }

        let event = StateChangeEvent::connection_change(tv_id, true, None);
        self.emit_state_change(event);
        info!(tv_id = %tv_id, "TV is online after WoL");
        Ok(())
    }

    /// Power off a TV.
    ///
    /// After sending the power-off command, clears the daemon's connection
    /// tracking. The backend already closed its own socket.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let result = self
            .with_connection(tv_id, |conn| {
                Box::pin(async move { conn.power_off().await })
            })
            .await;

        // Always drop the connection: if the command failed the socket is
        // likely broken anyway. Only mark as powered-off when the command
        // actually succeeded — if it failed the TV may still be on, and
        // leaving powered_off=false lets auto-reconnect attempt recovery.
        {
            let tv_arc = self.get_tv(tv_id).await?;
            let mut tv = tv_arc.lock().await;
            tv.connection = None;
            if result.is_ok() {
                tv.powered_off = true;
            }
        }

        result
    }

    /// Get power state.
    pub async fn get_power_state(&self, tv_id: &str) -> Result<PowerState, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_power_state().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Screen Control
    // -------------------------------------------------------------------------

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_on().await })
        })
        .await
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_off().await })
        })
        .await
    }

    /// Check if screen is on.
    pub async fn is_screen_on(&self, tv_id: &str) -> Result<bool, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.is_screen_on().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Volume Control
    // -------------------------------------------------------------------------

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_volume(volume).await })
        })
        .await
    }

    /// Get volume.
    pub async fn get_volume(&self, tv_id: &str) -> Result<u8, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_volume().await })
        })
        .await
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_mute(mute).await })
        })
        .await
    }

    /// Check if muted.
    pub async fn is_muted(&self, tv_id: &str) -> Result<bool, TvManagerError> {
        self.with_connection(tv_id, |conn| Box::pin(async move { conn.is_muted().await }))
            .await
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), TvManagerError> {
        let input = input_id.to_string();
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.switch_input(&input).await })
        })
        .await
    }

    /// Get current input.
    pub async fn get_current_input(&self, tv_id: &str) -> Result<Option<String>, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_current_input().await })
        })
        .await
    }

    /// Get available inputs.
    pub async fn get_inputs(&self, tv_id: &str) -> Result<Vec<(String, String)>, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move {
                let inputs = conn.list_inputs().await?;
                Ok(inputs.into_iter().map(|i| (i.id, i.label)).collect())
            })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // Backlight Control
    // -------------------------------------------------------------------------

    /// Set backlight.
    pub async fn set_backlight(&self, tv_id: &str, backlight: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_backlight(backlight).await })
        })
        .await
    }

    /// Get backlight.
    pub async fn get_backlight(&self, tv_id: &str) -> Result<u8, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_backlight().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    /// Get TV state.
    pub async fn get_state(&self, tv_id: &str) -> Result<TvState, TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.get_state().await })
        })
        .await
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// Pair a new TV.
    pub async fn pair_tv(
        &self,
        name: &str,
        vendor: &str,
        vendor_config: toml::Value,
        id: Option<&str>,
        managed: bool,
    ) -> Result<String, TvManagerError> {
        info!(name = %name, vendor = %vendor, "Pairing TV");

        // Start pairing via registry
        let mut session = self
            .registry
            .start_pairing(vendor, &vendor_config)
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        // Wait for pairing to complete
        let credentials = session
            .wait_for_completion()
            .await
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        // Build final vendor config from pairing results
        let final_config = self
            .registry
            .build_config_from_pairing(vendor, &vendor_config, &credentials)
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        // Generate ID from name if not provided
        let tv_id = id.map(String::from).unwrap_or_else(|| {
            name.to_lowercase()
                .replace(' ', "-")
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect()
        });

        // Create TV config with vendor-specific connection parameters
        let mut tv_config = TvConfig::new(&tv_id, name, vendor, managed);

        // Store the vendor config
        tv_config
            .vendor_config
            .insert(vendor.to_string(), final_config);

        // Connect with the new config
        let connection = self.registry.connect_tv(&tv_config).await.map_err(|e| {
            TvManagerError::ConnectionFailed {
                tv_id: tv_id.clone(),
                source: e,
            }
        })?;

        // Add to managed TVs (outer write lock -- modifying the HashMap)
        {
            let system = self.system_section.read().await;
            let system_profile = system.resolve_profile(
                &system,
                &tv_config.profile.system,
            );
            let mut tvs = self.tvs.write().await;
            let mut managed = ManagedTv::new(tv_config.clone(), system_profile);
            managed.connection = Some(connection);
            tvs.insert(
                tv_id.clone(),
                (tv_config.managed, Arc::new(Mutex::new(managed))),
            );
        }

        // Save config
        self.save_tv_config(&tv_config).await?;

        info!(tv_id = %tv_id, "TV paired successfully");
        Ok(tv_id)
    }

    /// Remove a TV.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<(), TvManagerError> {
        info!(tv_id = %tv_id, "Removing TV");

        // Remove from managed TVs (outer write lock -- modifying the HashMap)
        let removed_arc = {
            let mut tvs = self.tvs.write().await;
            tvs.shift_remove(tv_id).map(|(_, arc)| arc)
        };

        match removed_arc {
            Some(tv_arc) => {
                // Close connection if any (per-TV lock)
                let mut tv = tv_arc.lock().await;
                if let Some(mut conn) = tv.connection.take() {
                    let _ = conn.close().await;
                }
            }
            None => {
                return Err(TvManagerError::TvNotFound(tv_id.to_string()));
            }
        }

        // Remove from config file
        self.delete_tv_config(tv_id).await?;

        info!(tv_id = %tv_id, "TV removed successfully");
        Ok(())
    }

    /// Save a TV config to the configuration file.
    ///
    /// Serializes disk I/O via `disk_write_lock` to prevent concurrent load-modify-save
    /// races. Uses `spawn_blocking` to avoid blocking the async executor.
    async fn save_tv_config(&self, tv_config: &TvConfig) -> Result<(), TvManagerError> {
        let config_path = self.config_path.clone();
        let tv_config = tv_config.clone();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            config.upsert_tv(tv_config);
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))?
    }

    /// Delete a TV from the configuration file.
    ///
    /// Serializes disk I/O via `disk_write_lock` to prevent concurrent load-modify-save
    /// races. Uses `spawn_blocking` to avoid blocking the async executor.
    async fn delete_tv_config(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let config_path = self.config_path.clone();
        let tv_id = tv_id.to_string();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            config.remove_tv(&tv_id);
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))?
    }

    /// Set a system-level TV option and persist to disk.
    ///
    /// Applies the change to the in-memory TV config first (which also validates
    /// the TV ID and key), then persists via `spawn_blocking` to avoid blocking
    /// the async executor on disk I/O.
    pub async fn set_tv_option(
        &self,
        tv_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), TvManagerError> {
        // Validate and apply to in-memory TV config first.
        let tv_arc = self.get_tv(tv_id).await?;
        {
            let mut managed = tv_arc.lock().await;
            apply_tv_option(&mut managed.config, key, value)?;

            // Re-resolve the system profile when the profile assignment changes.
            if key == "profile.system" {
                let system = self.system_section.read().await;
                managed.system_profile = system.resolve_profile(
                    &system,
                    &managed.config.profile.system,
                );
            }
        } // per-TV lock released before I/O

        // Keep the outer managed flag in sync when toggled via SetTvOption.
        if key == "managed" {
            if let Ok(managed_val) = value.parse::<bool>() {
                let mut tvs = self.tvs.write().await;
                if let Some((flag, _)) = tvs.get_mut(tv_id) {
                    *flag = managed_val;
                }
            }
        }

        // Persist to disk: load the full on-disk config, patch the TV entry, save.
        // Serialize via disk_write_lock to prevent concurrent load-modify-save races.
        let config_path = self.config_path.clone();
        let tv_id_owned = tv_id.to_string();
        let key_owned = key.to_string();
        let value_owned = value.to_string();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            let tv = config
                .find_tv_mut(&tv_id_owned)
                .ok_or_else(|| TvManagerError::TvNotFound(tv_id_owned.clone()))?;
            apply_tv_option(tv, &key_owned, &value_owned)?;
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))??;

        info!(tv_id = %tv_id, key = %key, value = %value, "TV option updated");
        Ok(())
    }

    /// Reload configuration from disk.
    ///
    /// Uses `spawn_blocking` to avoid blocking the async executor on disk I/O,
    /// consistent with all other config-load sites in this file.
    pub async fn reload_config(&self) -> Result<(), TvManagerError> {
        info!("Reloading configuration");

        let config_path = self.config_path.clone();
        let _disk_lock = self.disk_write_lock.lock().await;
        let new_config = tokio::task::spawn_blocking(move || {
            SystemConfig::load_from(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to load config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))??;

        let new_ids: std::collections::HashSet<_> =
            new_config.tv.iter().map(|t| t.id.clone()).collect();

        // Phase 1: update the IndexMap under write lock (map mutations only — no I/O).
        // Collect Arcs for removed TVs and existing TVs that need config updates.
        // We do NOT close connections here; the write lock must not be held across
        // async WebSocket close operations that can block for hundreds of milliseconds.
        let mut to_update: Vec<(Arc<Mutex<ManagedTv>>, Option<TvConfig>)> = Vec::new();
        let tv_count;
        {
            let mut tvs = self.tvs.write().await;

            // Remove TVs that are no longer in config
            let old_ids: Vec<_> = tvs.keys().cloned().collect();
            for old_id in old_ids {
                if !new_ids.contains(&old_id) {
                    info!(tv_id = %old_id, "Removing TV (no longer in config)");
                    if let Some((_, arc)) = tvs.shift_remove(&old_id) {
                        to_update.push((arc, None)); // None = removed, close connection
                    }
                }
            }

            // Add or update TVs from new config
            for tv_config in new_config.tv {
                if let Some((managed_flag, tv_arc)) = tvs.get_mut(&tv_config.id) {
                    // Existing TV: update outer managed flag immediately under the write
                    // lock, then defer the rest of the config update to Phase 2.
                    *managed_flag = tv_config.managed;
                    to_update.push((tv_arc.clone(), Some(tv_config)));
                } else {
                    // New TV: add to map immediately (no connection to close).
                    info!(tv_id = %tv_config.id, "Adding new TV from config");
                    let system_profile = system.resolve_profile(
                        &new_config.system,
                        &tv_config.profile.system,
                    );
                    tvs.insert(
                        tv_config.id.clone(),
                        (
                            tv_config.managed,
                            Arc::new(Mutex::new(ManagedTv::new(tv_config, system_profile))),
                        ),
                    );
                }
            }

            tv_count = tvs.len();
        } // write lock released

        // Phase 2: update configs and close connections without holding the outer lock.
        // Each per-TV lock is held only for the duration of its own operation.
        for (tv_arc, new_tv_config) in to_update {
            let mut tv = tv_arc.lock().await;
            match new_tv_config {
                None => {
                    // TV removed from config — close its connection.
                    if let Some(mut conn) = tv.connection.take() {
                        let _ = conn.close().await;
                    }
                }
                Some(tv_config) => {
                    // Existing TV — reconnect if connection params changed.
                    let connection_changed = tv.config.vendor != tv_config.vendor
                        || tv.config.vendor_config != tv_config.vendor_config;
                    if connection_changed {
                        info!(tv_id = %tv_config.id, "Updating TV config (will reconnect)");
                        if let Some(mut conn) = tv.connection.take() {
                            let _ = conn.close().await;
                        }
                    }
                    tv.config = tv_config;
                }
            }
        }

        info!(tv_count = tv_count, "Configuration reloaded");

        // Update system section (system profiles)
        {
            let mut system = self.system_section.write().await;
            *system = new_config.system;
        }

        // Update user section (user profiles)
        {
            let mut user = self.user_section.write().await;
            *user = new_config.user;
        }

        // Phase 3: re-resolve system profiles for all TVs.
        // The system section may have changed, so every TV's resolved profile
        // needs to be recomputed from the new system profiles.
        {
            let system = self.system_section.read().await;
            let tvs = self.tvs.read().await;
            for (_, tv_arc) in tvs.values() {
                let mut tv = tv_arc.lock().await;
                tv.system_profile = system.resolve_profile(
                    &system,
                    &tv.config.profile.system,
                );
            }
        }

        Ok(())
    }
}
