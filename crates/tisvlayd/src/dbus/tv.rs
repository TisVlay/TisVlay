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

/// Base TV interface with properties and connection management.
///
/// Holds cached state that is updated by the subscription forwarding task
/// via `InterfaceRef::get_mut()`. Property reads are instant (no lock, no I/O).
pub struct TvService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached identity/config (set at registration, updated on config reload)
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) address: String,
    pub(crate) managed: bool,
    pub(crate) system_profile: String,

    // Cached runtime state (updated by subscription pushes)
    pub(crate) connected: bool,
}

#[interface(name = "org.tisvlay.Tv1")]
impl TvService {
    // -------------------------------------------------------------------------
    // Properties
    // -------------------------------------------------------------------------

    /// TV identifier.
    #[zbus(property(emits_changed_signal = "const"))]
    fn id(&self) -> &str {
        &self.tv_id
    }

    /// TV display name.
    #[zbus(property)]
    fn name(&self) -> &str {
        &self.name
    }

    /// TV vendor ID.
    #[zbus(property(emits_changed_signal = "const"))]
    fn vendor(&self) -> &str {
        &self.vendor
    }

    /// TV display address (vendor-specific, e.g. IP address).
    #[zbus(property(emits_changed_signal = "const"))]
    fn address(&self) -> &str {
        &self.address
    }

    /// Whether the TV is currently connected.
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.connected
    }

    /// Whether this TV participates in automated management.
    #[zbus(property)]
    fn managed(&self) -> bool {
        self.managed
    }

    /// System profile name assigned to this TV.
    #[zbus(property)]
    fn system_profile(&self) -> &str {
        &self.system_profile
    }

    /// Capabilities supported by this TV.
    ///
    /// Queried live from the backend (populated after connection).
    #[zbus(property(emits_changed_signal = "invalidates"))]
    async fn capabilities(&self) -> Vec<String> {
        self.manager
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
        self.manager
            .reconnect(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Get TV state as JSON.
    async fn get_state(&self) -> Result<String, DaemonError> {
        debug!(tv_id = %self.tv_id, "D-Bus: GetState");
        let state = self
            .manager
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
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) power_state: String,
}

#[interface(name = "org.tisvlay.Tv1.Power")]
impl TvPowerService {
    /// Power on the TV (Wake-on-LAN).
    async fn power_on(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: PowerOn");
        self.manager
            .power_on(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Power off the TV.
    async fn power_off(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: PowerOff");
        self.manager
            .power_off(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Get current power state.
    #[zbus(property)]
    fn power_state(&self) -> &str {
        &self.power_state
    }
}

// =============================================================================
// Screen control interface
// =============================================================================

/// Screen control interface (OLED-safe blanking).
pub struct TvScreenService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) is_screen_on: bool,
}

#[interface(name = "org.tisvlay.Tv1.Screen")]
impl TvScreenService {
    /// Turn the screen on (unblank).
    async fn screen_on(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: ScreenOn");
        self.manager
            .screen_on(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Turn the screen off (blank).
    async fn screen_off(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: ScreenOff");
        self.manager
            .screen_off(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Whether the screen is currently on.
    #[zbus(property)]
    fn screen_on_prop(&self) -> bool {
        self.is_screen_on
    }
}

// =============================================================================
// Volume control interface
// =============================================================================

/// Volume control interface.
pub struct TvVolumeService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) volume: u8,
    pub(crate) muted: bool,
}

#[interface(name = "org.tisvlay.Tv1.Volume")]
impl TvVolumeService {
    /// Set volume level (0-100).
    async fn set_volume(&self, level: u8) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, volume = level, "D-Bus: SetVolume");
        self.manager
            .set_volume(&self.tv_id, level)
            .await
            .map_err(DaemonError::from)
    }

    /// Current volume level (0-100).
    #[zbus(property)]
    fn volume(&self) -> u8 {
        self.volume
    }

    /// Set mute state.
    async fn set_mute(&self, mute: bool) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, mute = mute, "D-Bus: SetMute");
        self.manager
            .set_mute(&self.tv_id, mute)
            .await
            .map_err(DaemonError::from)
    }

    /// Whether audio is currently muted.
    #[zbus(property)]
    fn muted(&self) -> bool {
        self.muted
    }
}

// =============================================================================
// Input control interface
// =============================================================================

/// Input switching interface.
pub struct TvInputService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) current_input: String,
    pub(crate) available_inputs: Vec<(String, String)>,
}

#[interface(name = "org.tisvlay.Tv1.Input")]
impl TvInputService {
    /// Switch to the specified input.
    async fn switch_input(&self, input_id: &str) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, input_id = %input_id, "D-Bus: SwitchInput");
        self.manager
            .switch_input(&self.tv_id, input_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Currently active input ID.
    #[zbus(property)]
    fn current_input(&self) -> &str {
        &self.current_input
    }

    /// Available inputs as (id, label) tuples.
    #[zbus(property(emits_changed_signal = "invalidates"))]
    fn available_inputs(&self) -> Vec<(String, String)> {
        self.available_inputs.clone()
    }
}

// =============================================================================
// Backlight control interface
// =============================================================================

/// Backlight control interface (OLED pixel brightness).
pub struct TvBacklightService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) backlight: u8,
}

#[interface(name = "org.tisvlay.Tv1.Backlight")]
impl TvBacklightService {
    /// Set backlight level (0-100).
    async fn set_backlight(&self, level: u8) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, backlight = level, "D-Bus: SetBacklight");
        self.manager
            .set_backlight(&self.tv_id, level)
            .await
            .map_err(DaemonError::from)
    }

    /// Current backlight level (0-100).
    #[zbus(property)]
    fn backlight(&self) -> u8 {
        self.backlight
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
    // Ignore errors -- the interface may not be registered (e.g., added but never connected).
    let _ = object_server.remove::<TvBacklightService, _>(path).await;
    let _ = object_server.remove::<TvInputService, _>(path).await;
    let _ = object_server.remove::<TvVolumeService, _>(path).await;
    let _ = object_server.remove::<TvScreenService, _>(path).await;
    let _ = object_server.remove::<TvPowerService, _>(path).await;
    let _ = object_server.remove::<TvService, _>(path).await;
    Ok(())
}

/// Register all TV interfaces at the given object path.
///
/// Populates cached state from the manager's current config. Runtime state
/// (volume, power, etc.) starts at defaults and is filled in by subscription
/// pushes once the TV connects.
pub async fn register_tv_interfaces(
    connection: &zbus::Connection,
    path: &str,
    tv_id: String,
    manager: Arc<TvManager>,
) -> zbus::Result<()> {
    // Fetch config-based properties for initial cache
    let (name, vendor, address, managed) = manager
        .get_tv_info(&tv_id)
        .await
        .map(|info| (info.name, info.vendor, info.address, info.managed))
        .unwrap_or_else(|| (String::new(), String::new(), String::new(), true));
    let system_profile = manager
        .get_tv_system_profile(&tv_id)
        .await
        .unwrap_or_else(|| "default".to_string());
    let connected = manager.is_connected(&tv_id).await;

    let object_server = connection.object_server();

    object_server
        .at(
            path,
            TvService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                name,
                vendor,
                address,
                managed,
                system_profile,
                connected,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvPowerService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                power_state: "unknown".to_string(),
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvScreenService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                is_screen_on: true,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvVolumeService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                volume: 0,
                muted: false,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvInputService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                current_input: String::new(),
                available_inputs: Vec::new(),
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvBacklightService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                backlight: 0,
            },
        )
        .await?;

    Ok(())
}
