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
    Capabilities, ResolvedSystemProfile, SessionAction, SystemConfig, SystemSection, TvConfig,
    TvConnection, TvState, UserProfileConfig, UserSection,
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

/// Event emitted when TV state changes or daemon-wide events occur.
///
/// Routed through the state change channel to the forwarder task in main.rs,
/// which updates cached D-Bus properties via `InterfaceRef::get_mut()` and
/// emits `PropertiesChanged` signals.
#[derive(Debug, Clone)]
pub enum StateChangeEvent {
    /// Backend subscription push -- full TV state snapshot.
    ///
    /// The forwarder updates cached properties on all TV interface structs
    /// and emits `PropertiesChanged` for each changed property.
    SubscriptionUpdate { tv_id: String, state: TvState },
    /// Connection status changed (connected or disconnected).
    ///
    /// Emitted after successful connect/reconnect (connected=true) or when
    /// a backend subscription ends (connected=false).
    ConnectionChanged { tv_id: String, connected: bool },
    /// A TV config option was changed (managed, name, profile.system, etc.).
    ///
    /// The forwarder updates the corresponding cached property on TvService
    /// and emits `PropertiesChanged`.
    OptionChanged {
        tv_id: String,
        key: String,
        value: String,
    },
    /// Configuration was reloaded from disk.
    ///
    /// Profile lists, TV names, and managed flags may all differ.
    ConfigReloaded,
    /// A network interface's carrier went up (cable plugged in, link established).
    ///
    /// From kernel netlink RTMGRP_LINK. Triggers reconnection for TVs
    /// whose `monitored_interface` matches this interface.
    InterfaceUp { interface: String },
    /// A network interface's carrier went down (cable unplugged, link lost).
    ///
    /// From kernel netlink RTMGRP_LINK. Disconnects TVs whose
    /// `monitored_interface` matches this interface.
    InterfaceDown { interface: String },
    /// The kernel routing table changed.
    ///
    /// From kernel netlink RTMGRP_ROUTE. Re-resolves `monitored_interface`
    /// for all connected network TVs in case their route shifted to a
    /// different NIC (failover, DHCP renewal, VPN up/down).
    RouteChanged,
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
    /// A connection attempt is already in progress (WoL retry, on_wake, etc.).
    /// Prevents carrier/route events from racing with an existing attempt.
    reconnecting: bool,
    /// Resolved system profile for daemon lifecycle events.
    ///
    /// Resolved from the TV's `profile.system` assignment against the
    /// system profiles defined in the config. Updated on config reload
    /// and when the profile assignment changes via `set_tv_option`.
    system_profile: ResolvedSystemProfile,
    /// Last known current input from backend subscription pushes.
    ///
    /// Used by `is_automated()` to evaluate the input-aware gate without
    /// needing access to the D-Bus interface cache.
    last_known_input: Option<String>,
    /// NIC carrying traffic to this TV, resolved from the routing table.
    ///
    /// Set at connection time via `resolve_interface_for_ip()`. Used to
    /// scope carrier monitoring to the correct interface — only disconnect
    /// this TV when its specific NIC goes down, not when any NIC changes.
    monitored_interface: Option<String>,
}

impl ManagedTv {
    fn new(config: TvConfig, system_profile: ResolvedSystemProfile) -> Self {
        Self {
            config,
            connection: None,
            powered_off: false,
            reconnecting: false,
            system_profile,
            last_known_input: None,
            monitored_interface: None,
        }
    }

    /// Whether automation should fire for this TV right now.
    ///
    /// Composes `managed`, `automate_when` (from system profile), `primary_input`
    /// (from config), and `last_known_input` (from subscription pushes).
    fn is_automated(&self, managed: bool) -> bool {
        if !managed {
            return false;
        }
        self.system_profile.automate_when.should_automate(
            self.config.primary_input.as_deref(),
            self.last_known_input.as_deref(),
        )
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
pub enum SystemEvent {
    Startup,
    Shutdown,
    Reboot,
    NoSession,
    Sleep,
    Wake,
}

/// Apply a key-value TV option to a `TvConfig`.
///
/// Used by `set_tv_option` to update both the disk config and the in-memory
/// config with a single shared field-mapping, so neither can drift from the other.
fn apply_tv_option(tv: &mut TvConfig, key: &str, value: &str) -> Result<(), TvManagerError> {
    match key {
        "primary_input" => {
            tv.primary_input = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
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
    /// Try to subscribe to backend state updates for a TV.
    ///
    /// If the backend supports push-based state (e.g., LG SSAP subscriptions),
    /// spawns a background task that forwards state updates as StateChangeEvents.
    /// If not supported, does nothing (the tray's fallback poll handles it).
    async fn try_subscribe_state(&self, tv_id: &str, connection: &mut dyn TvConnection) {
        match connection.subscribe_state().await {
            Ok(mut state_rx) => {
                let tv_id = tv_id.to_string();
                let tx = self.state_change_tx.clone();
                let tvs = self.tvs.clone();
                info!(tv_id = %tv_id, "Subscribed to backend state updates");
                tokio::spawn(async move {
                    while let Some(state) = state_rx.recv().await {
                        // Update last_known_input on ManagedTv for the automation gate.
                        if let Some(ref input) = state.current_input {
                            let tvs_read = tvs.read().await;
                            if let Some((_, tv_arc)) = tvs_read.get(tv_id.as_str()) {
                                tv_arc.lock().await.last_known_input = Some(input.clone());
                            }
                        }
                        let event = StateChangeEvent::SubscriptionUpdate {
                            tv_id: tv_id.clone(),
                            state,
                        };
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    // Subscription ended -- TV likely disconnected.
                    let _ = tx.send(StateChangeEvent::ConnectionChanged {
                        tv_id: tv_id.clone(),
                        connected: false,
                    });
                    debug!(tv_id = %tv_id, "Backend state subscription ended");
                });
            }
            Err(_) => {
                // Backend doesn't support subscriptions -- this is fine,
                // the tray's fallback poll handles state refresh.
            }
        }
    }

    /// Get a clone of the state change sender for external event sources.
    pub fn state_change_sender(&self) -> mpsc::UnboundedSender<StateChangeEvent> {
        self.state_change_tx.clone()
    }

    /// Resolve and store the NIC carrying traffic to a TV.
    ///
    /// Parses the TV's display address as an IP, queries the kernel routing
    /// table, and stores the result on `ManagedTv::monitored_interface`.
    /// Only meaningful for network-transport TVs.
    async fn resolve_monitored_interface(&self, tv_id: &str) {
        let tv_arc = match self.get_tv(tv_id).await {
            Ok(arc) => arc,
            Err(_) => return,
        };
        let config = {
            let tv = tv_arc.lock().await;
            tv.config.clone()
        };

        if !self.registry.transport_for(&config).is_network_dependent() {
            return;
        }

        let address = self.registry.display_address(&config);
        let ip: std::net::IpAddr = match address.parse() {
            Ok(ip) => ip,
            Err(_) => {
                debug!(tv_id = %tv_id, address = %address, "Cannot parse TV address as IP for route resolution");
                return;
            }
        };

        let interface = crate::netlink::resolve_interface_for_ip(ip).await;
        let mut tv = tv_arc.lock().await;
        if tv.monitored_interface == interface {
            return;
        }

        let previous = tv.monitored_interface.clone();
        info!(tv_id = %tv_id, "Monitored interface: {previous:?} -> {interface:?}");
        tv.monitored_interface = interface.clone();

        // Should we tear down the existing connection?
        let should_disconnect = !tv.reconnecting
            && match (&previous, &interface) {
                // Route disappeared — TV is unreachable.
                (Some(_), None) => true,
                // Route shifted to a different NIC — close if the protocol
                // can't survive a source IP change (e.g., WebSocket over TCP).
                (Some(_), Some(_)) => {
                    tv.is_connected() && !self.registry.survives_source_ip_change(&config)
                }
                _ => false,
            };

        // Should we signal reconnection?
        let should_reconnect =
            interface.is_some() && !tv.is_connected() && !tv.powered_off && !tv.reconnecting;

        // Take connection outside the lock if disconnecting.
        let conn = if should_disconnect {
            tv.last_known_input = None;
            tv.connection.take()
        } else {
            None
        };

        drop(tv);

        if let Some(mut conn) = conn {
            let _ = conn.close().await;
            info!(tv_id = %tv_id, "Route changed ({previous:?} -> {interface:?}): disconnected");
            self.emit_state_change(StateChangeEvent::ConnectionChanged {
                tv_id: tv_id.to_string(),
                connected: false,
            });
        }

        if should_disconnect || should_reconnect {
            if let Some(iface) = interface {
                self.emit_state_change(StateChangeEvent::InterfaceUp { interface: iface });
            }
        }
    }

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
            SystemEvent::Sleep => tv.system_profile.on_sleep,
            SystemEvent::Wake => tv.system_profile.on_wake,
        }
    }

    /// List TV IDs where automation should fire (managed + input-aware gate).
    async fn list_automated_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        let mut result = Vec::new();
        for (id, (managed, tv_arc)) in tvs.iter() {
            let tv = tv_arc.lock().await;
            if tv.is_automated(*managed) {
                result.push(id.clone());
            }
        }
        result
    }

    /// Execute a system event on all automated TVs using each TV's resolved system profile.
    pub async fn execute_system_event(&self, event: SystemEvent, context: &str) {
        let tv_ids = self.list_automated_tv_ids().await;
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

    /// Connect to all configured TVs without executing startup actions.
    ///
    /// Used on warm restart (daemon process restart, not system boot).
    /// The TVs are likely already on — just re-establish WebSocket
    /// connections, subscribe to state updates, and resolve monitored
    /// interfaces. No WoL, no screen_on, no input switching.
    fn connect_all_on_restart(self: Arc<Self>) {
        tokio::spawn(async move {
            let tv_ids = self.list_tv_ids().await;
            for tv_id in &tv_ids {
                let display_addr = {
                    let tv_arc = match self.get_tv(tv_id).await {
                        Ok(arc) => arc,
                        Err(_) => continue,
                    };
                    let tv = tv_arc.lock().await;
                    self.registry.display_address(&tv.config)
                };

                info!(tv_id = %tv_id, address = %display_addr, "Warm restart: connecting to TV");

                match self.reconnect(tv_id).await {
                    Ok(()) => info!(tv_id = %tv_id, "Warm restart: connected"),
                    Err(e) => {
                        debug!(tv_id = %tv_id, error = %e, "Warm restart: connection failed (TV may be off)")
                    }
                }
            }
        });
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
            info!("Skipping on_startup actions (service restart, not system boot)");
            // Still connect to all TVs — they're likely already on from
            // before the restart. No WoL, no screen_on, no input switch.
            self.clone().connect_all_on_restart();
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

                // Store the connection and subscribe to state updates.
                {
                    let tvs = manager.tvs.read().await;
                    if let Some((_, tv_arc)) = tvs.get(tv_id.as_str()) {
                        let mut tv = tv_arc.lock().await;
                        tv.connection = Some(connection);
                        tv.powered_off = false;
                        if let Some(conn) = tv.connection.as_mut() {
                            manager.try_subscribe_state(tv_id, conn.as_mut()).await;
                        }
                    }
                }
                manager.emit_state_change(StateChangeEvent::ConnectionChanged {
                    tv_id: tv_id.to_string(),
                    connected: true,
                });
                manager.resolve_monitored_interface(tv_id).await;

                match manager.screen_on(tv_id).await {
                    Ok(()) => info!(tv_id = %tv_id, "Startup: screen on"),
                    Err(e) => {
                        warn!(tv_id = %tv_id, error = %e, "Startup: failed to turn screen on")
                    }
                }

                if let Some(ref input) = tv_config.primary_input {
                    match manager.switch_input(tv_id, input).await {
                        Ok(()) => info!(tv_id = %tv_id, input = %input, "Startup: switched input"),
                        Err(e) => {
                            warn!(tv_id = %tv_id, error = %e, "Startup: failed to switch input")
                        }
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

    /// Get the resolved `automate_when` policy for a TV.
    pub async fn get_tv_automate_when(&self, tv_id: &str) -> Option<tisvlay_core::AutomateWhen> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.system_profile.automate_when)
    }

    /// Get the primary input for a TV (from config).
    pub async fn get_tv_primary_input(&self, tv_id: &str) -> Option<String> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        tv.config.primary_input.clone()
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

        // Subscribe to backend state updates if supported
        if let Some(conn) = tv.connection.as_mut() {
            self.try_subscribe_state(tv_id, conn.as_mut()).await;
        }

        drop(tv);
        self.emit_state_change(StateChangeEvent::ConnectionChanged {
            tv_id: tv_id.to_string(),
            connected: true,
        });
        self.resolve_monitored_interface(tv_id).await;

        Ok(())
    }

    /// Reconnect network-dependent TVs that are currently disconnected.
    ///
    /// Called when network connectivity is restored (e.g., after a network
    /// blip or NIC reset). Reconnects all TVs regardless of managed status —
    /// managed gates automation (actions), not connectivity. Skips non-network
    /// transports (serial, USB, CEC), TVs that are intentionally powered off,
    /// and TVs with an in-flight WoL attempt. Each reconnection runs
    /// concurrently via spawned tasks.
    /// Disconnect TVs whose monitored interface matches the given name.
    ///
    /// Called when a specific NIC's carrier goes down. Only affects TVs
    /// routed through that interface — other TVs are untouched.
    /// Close all TV connections gracefully.
    ///
    /// Called during daemon shutdown so TVs drop their SSAP sessions
    /// immediately. Without this, the TV holds the stale session until
    /// TCP keepalive times out, causing "Connection reset" on warm restart.
    pub async fn close_all_connections(&self) {
        let mut to_close: Vec<(String, Box<dyn TvConnection>)> = Vec::new();
        {
            let tvs = self.tvs.read().await;
            for (tv_id, (_, tv_arc)) in tvs.iter() {
                let mut tv = tv_arc.lock().await;
                if let Some(conn) = tv.connection.take() {
                    to_close.push((tv_id.clone(), conn));
                }
            }
        }
        for (tv_id, mut conn) in to_close {
            debug!(tv_id = %tv_id, "Closing connection");
            let _ = conn.close().await;
        }
    }

    pub async fn disconnect_tvs_on_interface(&self, interface: &str) {
        let mut to_close: Vec<(String, Box<dyn TvConnection>)> = Vec::new();

        {
            let tvs = self.tvs.read().await;
            for (tv_id, (_, tv_arc)) in tvs.iter() {
                let mut tv = tv_arc.lock().await;
                if tv.monitored_interface.as_deref() != Some(interface) {
                    continue;
                }
                if tv.reconnecting {
                    debug!(tv_id = %tv_id, "Skipping disconnect, WoL already in progress");
                    continue;
                }
                if let Some(conn) = tv.connection.take() {
                    tv.last_known_input = None;
                    to_close.push((tv_id.clone(), conn));
                }
            }
        }

        if to_close.is_empty() {
            debug!(interface = %interface, "No TVs monitored on this interface");
        }
        for (tv_id, mut conn) in to_close {
            let _ = conn.close().await;
            info!(tv_id = %tv_id, "Carrier lost on {interface}: disconnected");
            self.emit_state_change(StateChangeEvent::ConnectionChanged {
                tv_id,
                connected: false,
            });
        }
    }

    /// Reconnect TVs whose monitored interface matches the given name.
    ///
    /// Called when a specific NIC's carrier comes back up. Only attempts
    /// reconnection for TVs that were routed through that interface.
    pub async fn reconnect_tvs_on_interface(self: &Arc<Self>, interface: &str) {
        let tvs = self.tvs.read().await;
        for (tv_id, (_, tv_arc)) in tvs.iter() {
            let tv = tv_arc.lock().await;
            if tv.monitored_interface.as_deref() != Some(interface) {
                continue;
            }
            if tv.is_connected() || tv.powered_off || tv.reconnecting {
                continue;
            }
            let tv_id = tv_id.clone();
            let iface = interface.to_owned();
            let manager = Arc::clone(self);
            drop(tv);
            tokio::spawn(async move {
                info!(tv_id = %tv_id, "Carrier restored on {iface}: reconnecting");
                if let Err(e) = manager.reconnect(&tv_id).await {
                    debug!(tv_id = %tv_id, error = %e, "Reconnect failed (TV may be off)");
                }
            });
        }
    }

    /// Re-resolve `monitored_interface` for all connected network TVs.
    ///
    /// Called when the kernel routing table changes. If a TV's route shifted
    /// to a different NIC (failover, DHCP renewal), updates the mapping so
    /// carrier monitoring tracks the correct interface.
    pub async fn re_resolve_all_interfaces(&self) {
        let tv_ids: Vec<String> = {
            let tvs = self.tvs.read().await;
            tvs.keys().cloned().collect()
        };
        for tv_id in &tv_ids {
            self.resolve_monitored_interface(tv_id).await;
        }
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
            self.emit_state_change(StateChangeEvent::ConnectionChanged {
                tv_id: tv_id.to_string(),
                connected: true,
            });

            // Resolve which NIC routes to this TV (inline — we hold the lock)
            if self
                .registry
                .transport_for(&tv.config)
                .is_network_dependent()
            {
                let address = self.registry.display_address(&tv.config);
                if let Ok(ip) = address.parse::<std::net::IpAddr>() {
                    let interface = crate::netlink::resolve_interface_for_ip(ip).await;
                    if tv.monitored_interface != interface {
                        debug!(tv_id = %tv_id, interface = ?interface, "Monitored interface resolved");
                        tv.monitored_interface = interface;
                    }
                }
            }

            // Subscribe to backend state updates if supported
            if let Some(conn) = tv.connection.as_mut() {
                self.try_subscribe_state(tv_id, conn.as_mut()).await;
            }
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
            tv.reconnecting = true;
            if let Some(mut conn) = tv.connection.take() {
                let _ = conn.close().await;
            }

            tv.config.clone()
        };

        // Backend handles wake + boot wait + connect retry (no lock held)
        let result = self.registry.wake_and_connect_tv(&config).await;

        // Store the connection and subscribe to state updates
        {
            let mut tv = tv_arc.lock().await;
            tv.reconnecting = false;
            match result {
                Ok(connection) => {
                    tv.connection = Some(connection);
                    if let Some(conn) = tv.connection.as_mut() {
                        self.try_subscribe_state(tv_id, conn.as_mut()).await;
                    }
                }
                Err(e) => {
                    return Err(TvManagerError::WakeFailed {
                        tv_id: tv_id.to_string(),
                        source: e,
                    });
                }
            }
        }

        self.emit_state_change(StateChangeEvent::ConnectionChanged {
            tv_id: tv_id.to_string(),
            connected: true,
        });
        self.resolve_monitored_interface(tv_id).await;
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

    // -------------------------------------------------------------------------
    // Screen Control
    // -------------------------------------------------------------------------

    /// Turn screen on.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_on().await })
        })
        .await?;
        Ok(())
    }

    /// Turn screen off.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.screen_off().await })
        })
        .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Volume Control
    // -------------------------------------------------------------------------

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_volume(volume).await })
        })
        .await?;
        Ok(())
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_mute(mute).await })
        })
        .await?;
        Ok(())
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
        .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Backlight Control
    // -------------------------------------------------------------------------

    /// Set backlight.
    pub async fn set_backlight(&self, tv_id: &str, backlight: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_backlight(backlight).await })
        })
        .await?;
        Ok(())
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

        // Add to managed TVs and subscribe to state updates
        {
            let system = self.system_section.read().await;
            let system_profile = system.resolve_profile(&tv_config.profile.system);
            let mut tvs = self.tvs.write().await;
            let mut managed = ManagedTv::new(tv_config.clone(), system_profile);
            managed.connection = Some(connection);
            if let Some(conn) = managed.connection.as_mut() {
                self.try_subscribe_state(&tv_id, conn.as_mut()).await;
            }
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
        // Validate system profile name exists before applying.
        if key == "profile.system" {
            let system = self.system_section.read().await;
            if !system.profiles.contains_key(value) {
                return Err(TvManagerError::ConfigError(format!(
                    "Unknown system profile: {}",
                    value
                )));
            }
        }

        let tv_arc = self.get_tv(tv_id).await?;
        {
            let mut managed = tv_arc.lock().await;
            apply_tv_option(&mut managed.config, key, value)?;

            // Re-resolve the system profile when the assignment changes.
            if key == "profile.system" {
                let system = self.system_section.read().await;
                managed.system_profile = system.resolve_profile(&managed.config.profile.system);
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

        self.emit_state_change(StateChangeEvent::OptionChanged {
            tv_id: tv_id.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        });
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
                    let system_profile =
                        new_config.system.resolve_profile(&tv_config.profile.system);
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
                tv.system_profile = system.resolve_profile(&tv.config.profile.system);
            }
        }

        // Notify signal subscribers that configuration changed
        // (profile lists, TV names, managed flags may all differ).
        self.emit_state_change(StateChangeEvent::ConfigReloaded);

        Ok(())
    }
}
