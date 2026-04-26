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
use tisvlay_core::TvState;
use tracing::info;
use zbus::interface;
use zbus::object_server::SignalEmitter;

/// Base TV interface with properties and connection management.
///
/// Holds cached state that is updated by the subscription forwarding task
/// via `InterfaceRef::get_mut()`. Property reads are instant (no lock, no I/O).
#[derive(Debug)]
pub struct TvService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,

    // Cached identity/config (set at registration, updated on config reload)
    pub(crate) name: String,
    pub(crate) vendor: String,
    pub(crate) address: String,
    pub(crate) managed: bool,
    pub(crate) system_profile: String,
    pub(crate) primary_input: String,
    pub(crate) automate_when: tisvlay_core::AutomateWhen,

    // Cached runtime state (updated by subscription pushes)
    pub(crate) connected: bool,
    /// Copy of current_input from TvInputService, used to compute `Automated`.
    pub(crate) current_input: String,
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

    /// TV display address (vendor-specific, e.g. IP address, serial port path).
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

    /// The input this PC is connected to (e.g., "HDMI_1").
    #[zbus(property)]
    fn primary_input(&self) -> &str {
        &self.primary_input
    }

    /// Set the primary input for this TV.
    #[zbus(property)]
    async fn set_primary_input(&mut self, input: String) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, input = %input, "D-Bus: set PrimaryInput");
        self.manager
            .set_tv_option(&self.tv_id, "primary_input", &input)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.primary_input = input;
        Ok(())
    }

    /// When automation fires relative to the TV's current input.
    ///
    /// From the resolved system profile: "always", "on_primary", "off_primary".
    #[zbus(property)]
    fn automate_when(&self) -> String {
        self.automate_when.to_string()
    }

    /// Whether automation should fire for this TV right now.
    ///
    /// Computed from `managed`, `automate_when`, `primary_input`, and the
    /// TV's current input. When the TV is off (no known input), returns
    /// `true` (benefit of the doubt).
    #[zbus(property)]
    fn automated(&self) -> bool {
        if !self.managed {
            return false;
        }
        let primary = (!self.primary_input.is_empty()).then_some(self.primary_input.as_str());
        let current = (!self.current_input.is_empty()).then_some(self.current_input.as_str());
        self.automate_when.should_automate(primary, current)
    }

    /// Vendor-reported hardware identity: `(manufacturer, model, serial)`.
    ///
    /// Live-read from the backend's cached identity (empty strings for
    /// unknown fields). Emits `invalidates` rather than a full value
    /// change signal so clients re-fetch on the next access after the
    /// backend connects and populates its cache.
    #[zbus(property(emits_changed_signal = "invalidates"))]
    async fn hardware_id(&self) -> (String, String, String) {
        self.manager
            .get_hardware_id(&self.tv_id)
            .await
            .unwrap_or_default()
            .to_dbus_tuple()
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
#[derive(Debug)]
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

    /// Phase transitions during a power-on flow.
    ///
    /// Emitted as the manager walks through wake-on-LAN and connect
    /// retry. Phase values: `wol_sent`, `connecting`, `connected`,
    /// `failed` (see [`tisvlay_core::PowerCyclePhase`]). GUIs subscribe
    /// to render staged feedback during the 5-30 s wake window.
    #[zbus(signal)]
    pub async fn power_cycle_progress(emitter: &SignalEmitter<'_>, phase: &str)
    -> zbus::Result<()>;
}

// =============================================================================
// Screen control interface
// =============================================================================

/// Screen control interface (OLED-safe blanking).
///
/// The screen state is exposed as a writable boolean property `IsScreenOn`.
/// Setting `true` unblanks the screen, setting `false` blanks it.
#[derive(Debug)]
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
        // No optimistic pre-cache: state_forwarder updates the cache
        // from TV-side subscription pushes. See `set_backlight` for
        // the full rationale.
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
#[derive(Debug)]
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
        // No optimistic pre-cache: state_forwarder updates the cache
        // from TV-side subscription pushes. See `set_backlight` for
        // the full rationale.
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
        // No optimistic pre-cache: state_forwarder updates the cache
        // from TV-side subscription pushes.
        Ok(())
    }
}

// =============================================================================
// Input control interface
// =============================================================================

/// Input switching interface.
#[derive(Debug)]
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
#[derive(Debug)]
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
        // Dispatch and return — do NOT pre-cache `self.backlight`.
        // The cache is the daemon's view of TV-actual state, populated
        // by `state_forwarder` from subscription pushes. Pre-caching
        // here lets zbus's auto-emit on `Properties.Set` race ahead
        // of the subscription path: clients see a `PropertiesChanged`
        // with the user's target value BEFORE the TV has actually
        // reached it, which (for ramping properties like backlight)
        // breaks convergence-gating clients — they treat the early
        // emit as "daemon converged", clear `pending`, and apply
        // subsequent ramp values as external changes (visible thumb
        // snap-back).
        //
        // Same reasoning applied to `set_volume` / `set_muted` /
        // `set_is_screen_on`. They didn't visibly manifest the bug
        // because LG applies those instantaneously (subscription push
        // arrives before zbus's auto-emit), but the inconsistency was
        // a footgun. See
        // `crates/tisvlayd/tests/backlight_emit_ordering.rs`.
        self.manager
            .set_backlight(&self.tv_id, level)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        Ok(())
    }
}

// =============================================================================
// Desktop mapping interface
// =============================================================================

/// Desktop identity for a TV.
///
/// All properties are live-read from the manager's in-memory mapping
/// cache, which is populated by the daemon's in-process desktop probe
/// (see [`crate::desktop_probe`]). Empty strings mean unknown / unset.
/// Emits `invalidates` on change so clients re-read on next access.
#[derive(Debug)]
pub struct TvDesktopService {
    pub(crate) tv_id: String,
    pub(crate) manager: Arc<TvManager>,
}

#[interface(name = "org.tisvlay.Tv1.Desktop")]
impl TvDesktopService {
    /// DRM connector name the daemon's desktop probe matched this TV
    /// to (empty when unknown / not yet probed).
    ///
    /// Tooling that wants richer EDID detail (capability tables,
    /// manufacture date, vendor blocks) reads this connector name and
    /// parses `/sys/class/drm/<connector>/edid` itself via the probe
    /// library — D-Bus carries only the matching outcome.
    #[zbus(property)]
    async fn connector(&self) -> String {
        self.manager
            .get_desktop_mapping(&self.tv_id)
            .await
            .map(|m| m.connector)
            .unwrap_or_default()
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
    let _ = object_server.remove::<TvDesktopService, _>(path).await;
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
    initial_state: Option<&TvState>,
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
    let primary_input = manager
        .get_tv_primary_input(&tv_id)
        .await
        .unwrap_or_default();
    let automate_when = manager
        .get_tv_automate_when(&tv_id)
        .await
        .unwrap_or_default();
    let connected = manager.is_connected(&tv_id).await;

    // Use live backend state if available, otherwise fall back to
    // safe defaults. This avoids the race where subscription updates
    // from try_subscribe_state() arrive before D-Bus interfaces are
    // registered and are silently dropped by the forwarder.
    let power_state = initial_state
        .map(|s| {
            match (s.is_on, s.is_screen_on) {
                (false, _) => "standby",
                (true, false) => "screen_off",
                (true, true) => "active",
            }
            .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string());
    let is_screen_on = initial_state.map(|s| s.is_screen_on).unwrap_or(true);
    let volume = initial_state.and_then(|s| s.volume).unwrap_or(0);
    let muted = initial_state.and_then(|s| s.muted).unwrap_or(false);
    let current_input = initial_state
        .and_then(|s| s.current_input.clone())
        .unwrap_or_default();
    let available_inputs: Vec<(String, String)> = initial_state
        .map(|s| {
            s.inputs
                .iter()
                .map(|i| (i.id.clone(), i.label.clone()))
                .collect()
        })
        .unwrap_or_default();
    let backlight = initial_state.and_then(|s| s.backlight).unwrap_or(0);

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
                primary_input,
                automate_when,
                connected,
                current_input: current_input.clone(),
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvPowerService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                power_state,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvScreenService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                is_screen_on,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvVolumeService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                volume,
                muted,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvInputService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                current_input,
                available_inputs,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvBacklightService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                backlight,
            },
        )
        .await?;

    object_server
        .at(
            path,
            TvDesktopService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
            },
        )
        .await?;

    Ok(())
}
