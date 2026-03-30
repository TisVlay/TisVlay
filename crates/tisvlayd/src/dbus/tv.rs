//! D-Bus TV interface implementation.
//!
//! Implements per-TV interfaces at `/org/tisvlay/Tv/{id}`:
//! - `org.tisvlay.Tv1` - Base interface (properties, connection status)
//! - `org.tisvlay.Tv1.Power` - Power control
//! - `org.tisvlay.Tv1.Screen` - Screen blanking
//! - `org.tisvlay.Tv1.Volume` - Volume and mute
//! - `org.tisvlay.Tv1.Input` - Input switching
//! - `org.tisvlay.Tv1.Backlight` - Backlight control

use crate::dbus::DaemonError;
use crate::tv_manager::TvManager;
use std::sync::Arc;
use tracing::{debug, info};
use zbus::interface;
use zbus::object_server::SignalEmitter;

/// Shared state for all TV interfaces.
type TvState = (String, Arc<TvManager>);

/// Base TV interface with properties and connection management.
pub struct TvService {
    tv_id: String,
    manager: Arc<TvManager>,
}

impl TvService {
    /// Create a new TV service.
    pub fn new(tv_id: String, manager: Arc<TvManager>) -> Self {
        Self { tv_id, manager }
    }

    /// Get the shared state for creating feature interfaces.
    pub fn state(&self) -> TvState {
        (self.tv_id.clone(), self.manager.clone())
    }
}

#[interface(name = "org.tisvlay.Tv1")]
impl TvService {
    // -------------------------------------------------------------------------
    // Properties
    // -------------------------------------------------------------------------

    /// TV identifier.
    #[zbus(property)]
    async fn id(&self) -> String {
        self.tv_id.clone()
    }

    /// TV display name.
    #[zbus(property)]
    async fn name(&self) -> String {
        let manager = &self.manager;
        manager
            .get_tv_info(&self.tv_id)
            .await
            .map(|info| info.name)
            .unwrap_or_default()
    }

    /// TV vendor ID.
    #[zbus(property)]
    async fn vendor(&self) -> String {
        let manager = &self.manager;
        manager
            .get_tv_info(&self.tv_id)
            .await
            .map(|info| info.vendor)
            .unwrap_or_default()
    }

    /// TV display address (vendor-specific, e.g. IP address).
    #[zbus(property)]
    async fn address(&self) -> String {
        let manager = &self.manager;
        manager
            .get_tv_info(&self.tv_id)
            .await
            .map(|info| info.address)
            .unwrap_or_default()
    }

    /// Whether the TV is currently connected.
    #[zbus(property)]
    async fn connected(&self) -> bool {
        let manager = &self.manager;
        manager.is_connected(&self.tv_id).await
    }

    /// Whether this TV participates in automated management.
    #[zbus(property)]
    async fn managed(&self) -> bool {
        let manager = &self.manager;
        manager
            .get_tv_info(&self.tv_id)
            .await
            .map(|info| info.managed)
            .unwrap_or(true)
    }

    /// System profile name assigned to this TV.
    #[zbus(property)]
    async fn system_profile(&self) -> String {
        let manager = &self.manager;
        manager
            .get_tv_system_profile(&self.tv_id)
            .await
            .unwrap_or_else(|| "default".to_string())
    }

    /// Capabilities supported by this TV.
    #[zbus(property)]
    async fn capabilities(&self) -> Vec<String> {
        let manager = &self.manager;
        manager
            .get_capabilities(&self.tv_id)
            .await
            .map(|caps| caps.to_strings().into_iter().map(String::from).collect())
            .unwrap_or_default()
    }

    // -------------------------------------------------------------------------
    // Methods
    // -------------------------------------------------------------------------

    /// Force reconnection to the TV.
    async fn reconnect(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: Reconnect");
        let manager = &self.manager;
        manager
            .reconnect(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Get TV state as JSON.
    async fn get_state(&self) -> Result<String, DaemonError> {
        debug!(tv_id = %self.tv_id, "D-Bus: GetState");
        let manager = &self.manager;
        let state = manager
            .get_state(&self.tv_id)
            .await
            .map_err(DaemonError::from)?;
        serde_json::to_string(&state)
            .map_err(|e| DaemonError::OperationFailed(format!("JSON serialization failed: {}", e)))
    }

    // -------------------------------------------------------------------------
    // Signals
    // -------------------------------------------------------------------------

    /// Signal emitted when TV state changes.
    #[zbus(signal)]
    pub async fn state_changed(emitter: &SignalEmitter<'_>, state: &str) -> zbus::Result<()>;

    /// Signal emitted when connection status changes.
    #[zbus(signal)]
    pub async fn connection_changed(
        emitter: &SignalEmitter<'_>,
        connected: bool,
    ) -> zbus::Result<()>;
}

// =============================================================================
// Power control interface
// =============================================================================

/// Power control interface.
pub struct TvPowerService {
    tv_id: String,
    manager: Arc<TvManager>,
}

impl TvPowerService {
    /// Create a new power service from shared state.
    pub fn from_state(state: &TvState) -> Self {
        Self {
            tv_id: state.0.clone(),
            manager: state.1.clone(),
        }
    }
}

#[interface(name = "org.tisvlay.Tv1.Power")]
impl TvPowerService {
    /// Power on the TV (Wake-on-LAN).
    async fn power_on(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: PowerOn");
        let manager = &self.manager;
        manager
            .power_on(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Power off the TV.
    async fn power_off(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: PowerOff");
        let manager = &self.manager;
        manager
            .power_off(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Get current power state.
    #[zbus(property)]
    async fn power_state(&self) -> String {
        let manager = &self.manager;
        manager
            .get_power_state(&self.tv_id)
            .await
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

// =============================================================================
// Screen control interface
// =============================================================================

/// Screen control interface (OLED-safe blanking).
pub struct TvScreenService {
    tv_id: String,
    manager: Arc<TvManager>,
}

impl TvScreenService {
    /// Create a new screen service from shared state.
    pub fn from_state(state: &TvState) -> Self {
        Self {
            tv_id: state.0.clone(),
            manager: state.1.clone(),
        }
    }
}

#[interface(name = "org.tisvlay.Tv1.Screen")]
impl TvScreenService {
    /// Turn the screen on (unblank).
    async fn screen_on(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: ScreenOn");
        let manager = &self.manager;
        manager
            .screen_on(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Turn the screen off (blank).
    async fn screen_off(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: ScreenOff");
        let manager = &self.manager;
        manager
            .screen_off(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Whether the screen is currently on.
    #[zbus(property)]
    async fn screen_on_prop(&self) -> bool {
        let manager = &self.manager;
        manager.is_screen_on(&self.tv_id).await.unwrap_or(true)
    }
}

// =============================================================================
// Volume control interface
// =============================================================================

/// Volume control interface.
pub struct TvVolumeService {
    tv_id: String,
    manager: Arc<TvManager>,
}

impl TvVolumeService {
    /// Create a new volume service from shared state.
    pub fn from_state(state: &TvState) -> Self {
        Self {
            tv_id: state.0.clone(),
            manager: state.1.clone(),
        }
    }
}

#[interface(name = "org.tisvlay.Tv1.Volume")]
impl TvVolumeService {
    /// Set volume level (0-100).
    async fn set_volume(&self, level: u8) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, volume = level, "D-Bus: SetVolume");
        let manager = &self.manager;
        manager
            .set_volume(&self.tv_id, level)
            .await
            .map_err(DaemonError::from)
    }

    /// Current volume level (0-100).
    #[zbus(property)]
    async fn volume(&self) -> u8 {
        let manager = &self.manager;
        manager.get_volume(&self.tv_id).await.unwrap_or(0)
    }

    /// Set mute state.
    async fn set_mute(&self, mute: bool) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, mute = mute, "D-Bus: SetMute");
        let manager = &self.manager;
        manager
            .set_mute(&self.tv_id, mute)
            .await
            .map_err(DaemonError::from)
    }

    /// Whether audio is currently muted.
    #[zbus(property)]
    async fn muted(&self) -> bool {
        let manager = &self.manager;
        manager.is_muted(&self.tv_id).await.unwrap_or(false)
    }
}

// =============================================================================
// Input control interface
// =============================================================================

/// Input switching interface.
pub struct TvInputService {
    tv_id: String,
    manager: Arc<TvManager>,
}

impl TvInputService {
    /// Create a new input service from shared state.
    pub fn from_state(state: &TvState) -> Self {
        Self {
            tv_id: state.0.clone(),
            manager: state.1.clone(),
        }
    }
}

#[interface(name = "org.tisvlay.Tv1.Input")]
impl TvInputService {
    /// Switch to the specified input.
    async fn switch_input(&self, input_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, input_id = %input_id, "D-Bus: SwitchInput");
        let manager = &self.manager;
        manager
            .switch_input(&self.tv_id, input_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Currently active input ID.
    #[zbus(property)]
    async fn current_input(&self) -> String {
        let manager = &self.manager;
        manager
            .get_current_input(&self.tv_id)
            .await
            .unwrap_or(None)
            .unwrap_or_default()
    }

    /// Available inputs as (id, label) tuples.
    #[zbus(property)]
    async fn available_inputs(&self) -> Vec<(String, String)> {
        let manager = &self.manager;
        manager.get_inputs(&self.tv_id).await.unwrap_or_default()
    }
}

// =============================================================================
// Backlight control interface
// =============================================================================

/// Backlight control interface (OLED pixel brightness).
pub struct TvBacklightService {
    tv_id: String,
    manager: Arc<TvManager>,
}

impl TvBacklightService {
    /// Create a new backlight service from shared state.
    pub fn from_state(state: &TvState) -> Self {
        Self {
            tv_id: state.0.clone(),
            manager: state.1.clone(),
        }
    }
}

#[interface(name = "org.tisvlay.Tv1.Backlight")]
impl TvBacklightService {
    /// Set backlight level (0-100).
    async fn set_backlight(&self, level: u8) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, backlight = level, "D-Bus: SetBacklight");
        let manager = &self.manager;
        manager
            .set_backlight(&self.tv_id, level)
            .await
            .map_err(DaemonError::from)
    }

    /// Current backlight level (0-100).
    #[zbus(property)]
    async fn backlight(&self) -> u8 {
        let manager = &self.manager;
        manager.get_backlight(&self.tv_id).await.unwrap_or(0)
    }
}

// =============================================================================
// Helper to register all interfaces
// =============================================================================

/// Unregister all TV interfaces at the given object path.
pub async fn unregister_tv_interfaces(
    connection: &zbus::Connection,
    path: &str,
) -> zbus::Result<()> {
    let object_server = connection.object_server();
    // Ignore errors — the interface may not be registered (e.g., added but never connected).
    let _ = object_server.remove::<TvBacklightService, _>(path).await;
    let _ = object_server.remove::<TvInputService, _>(path).await;
    let _ = object_server.remove::<TvVolumeService, _>(path).await;
    let _ = object_server.remove::<TvScreenService, _>(path).await;
    let _ = object_server.remove::<TvPowerService, _>(path).await;
    let _ = object_server.remove::<TvService, _>(path).await;
    Ok(())
}

/// Register all TV interfaces at the given object path.
pub async fn register_tv_interfaces(
    connection: &zbus::Connection,
    path: &str,
    tv_id: String,
    manager: Arc<TvManager>,
) -> zbus::Result<()> {
    let base = TvService::new(tv_id, manager);
    let state = base.state();

    let object_server = connection.object_server();

    // Register base interface
    object_server.at(path, base).await?;

    // Register feature interfaces
    object_server
        .at(path, TvPowerService::from_state(&state))
        .await?;
    object_server
        .at(path, TvScreenService::from_state(&state))
        .await?;
    object_server
        .at(path, TvVolumeService::from_state(&state))
        .await?;
    object_server
        .at(path, TvInputService::from_state(&state))
        .await?;
    object_server
        .at(path, TvBacklightService::from_state(&state))
        .await?;

    Ok(())
}
