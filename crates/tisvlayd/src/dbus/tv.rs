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
use tracing::info;
use zbus::interface;

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

    /// Set TV display name.
    #[zbus(property)]
    async fn set_name(&mut self, name: String) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, name = %name, "D-Bus: set Name");
        self.manager
            .set_tv_option(&self.tv_id, "name", &name)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.name = name;
        Ok(())
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

    /// Set whether this TV participates in automated management.
    #[zbus(property)]
    async fn set_managed(&mut self, managed: bool) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, managed = %managed, "D-Bus: set Managed");
        self.manager
            .set_tv_option(&self.tv_id, "managed", &managed.to_string())
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.managed = managed;
        Ok(())
    }

    /// System profile name assigned to this TV.
    #[zbus(property)]
    fn system_profile(&self) -> &str {
        &self.system_profile
    }

    /// Set the system profile name for this TV.
    #[zbus(property)]
    async fn set_system_profile(&mut self, profile: String) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, profile = %profile, "D-Bus: set SystemProfile");
        self.manager
            .set_tv_option(&self.tv_id, "profile.system", &profile)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.system_profile = profile;
        Ok(())
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
///
/// The screen state is exposed as a writable boolean property `IsScreenOn`.
/// Setting `true` unblanks the screen, setting `false` blanks it.
pub struct TvScreenService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) is_screen_on: bool,
}

#[interface(name = "org.tisvlay.Tv1.Screen")]
impl TvScreenService {
    /// Whether the screen is currently on.
    ///
    /// Set `true` to unblank, `false` to blank (OLED-safe screen off).
    #[zbus(property)]
    fn is_screen_on(&self) -> bool {
        self.is_screen_on
    }

    /// Set screen on/off state.
    #[zbus(property)]
    async fn set_is_screen_on(&mut self, on: bool) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, on = on, "D-Bus: set IsScreenOn");
        if on {
            self.manager.screen_on(&self.tv_id).await
        } else {
            self.manager.screen_off(&self.tv_id).await
        }
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.is_screen_on = on;
        Ok(())
    }
}

// =============================================================================
// Volume control interface
// =============================================================================

/// Volume control interface.
///
/// Volume and mute are writable properties. Setting them sends the command
/// to the TV backend and updates the cache on success.
pub struct TvVolumeService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) volume: u8,
    pub(crate) muted: bool,
}

#[interface(name = "org.tisvlay.Tv1.Volume")]
impl TvVolumeService {
    /// Current volume level (0-100).
    #[zbus(property)]
    fn volume(&self) -> u8 {
        self.volume
    }

    /// Set volume level (0-100).
    #[zbus(property)]
    async fn set_volume(&mut self, level: u8) -> zbus::fdo::Result<()> {
        if level > 100 {
            return Err(zbus::fdo::Error::InvalidArgs("Volume must be 0-100".into()));
        }
        info!(tv_id = %self.tv_id, volume = level, "D-Bus: set Volume");
        self.manager
            .set_volume(&self.tv_id, level)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.volume = level;
        Ok(())
    }

    /// Whether audio is currently muted.
    #[zbus(property)]
    fn muted(&self) -> bool {
        self.muted
    }

    /// Set mute state.
    #[zbus(property)]
    async fn set_muted(&mut self, muted: bool) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, muted = muted, "D-Bus: set Muted");
        self.manager
            .set_mute(&self.tv_id, muted)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.muted = muted;
        Ok(())
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
///
/// Backlight is a writable property. Setting it sends the command to the
/// TV backend and updates the cache on success.
pub struct TvBacklightService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached state
    pub(crate) backlight: u8,
}

#[interface(name = "org.tisvlay.Tv1.Backlight")]
impl TvBacklightService {
    /// Current backlight level (0-100).
    #[zbus(property)]
    fn backlight(&self) -> u8 {
        self.backlight
    }

    /// Set backlight level (0-100).
    #[zbus(property)]
    async fn set_backlight(&mut self, level: u8) -> zbus::fdo::Result<()> {
        if level > 100 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Backlight must be 0-100".into(),
            ));
        }
        info!(tv_id = %self.tv_id, backlight = level, "D-Bus: set Backlight");
        self.manager
            .set_backlight(&self.tv_id, level)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.backlight = level;
        Ok(())
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
