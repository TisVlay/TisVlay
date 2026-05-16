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
use std::str::FromStr;
use std::sync::Arc;
use tisvlay_core::dbus_paths::tv_path;
use tisvlay_core::{QosConfig, SessionAction, TvState, default_reliability_for};
use tracing::{debug, info, warn};
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
    pub(crate) managed: bool,
    pub(crate) system_profile: String,
    pub(crate) primary_input: String,
    pub(crate) automate_when: tisvlay_core::AutomateWhen,
    /// User-editable vendor-config rows — `(label, value)` pairs
    /// from the backend's `display_config` (LG: IP, MAC). Surfaced
    /// via `Tv1.VendorConfig` and rendered in the GUI's CONFIG
    /// group. Empty for backends with no `UserInput` schema fields.
    pub(crate) vendor_config: Vec<(String, String)>,
    /// Read-only diagnostic vendor rows — `(label, value)` pairs
    /// from the backend's `display_diagnostics` (LG: WebSocket
    /// scheme). Surfaced via `Tv1.VendorInfo` and rendered in the
    /// GUI's INFO group. Empty for backends with no `Diagnostic`
    /// schema fields.
    pub(crate) vendor_info: Vec<(String, String)>,

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

    /// TV UUID (RFC 4122). Stable, opaque, generated once at pair time.
    /// `Tv1.Name` is the user-facing label; this is the immutable handle.
    #[zbus(property(emits_changed_signal = "const"))]
    fn uuid(&self) -> &str {
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

    /// User-editable vendor-config rows as ordered `(label, value)`
    /// pairs. The GUI's CONFIG group renders these in view mode
    /// and the edit form drives the same field set in edit mode
    /// via `Tv1.UpdateConfig`. Schema-driven projection — see
    /// [`TvConnector::display_config`]. LG: IP + MAC. Future
    /// backends: whatever `UserInput`-visibility fields they
    /// declare on their `config_schema`.
    ///
    /// Set at interface-registration time and refreshed via
    /// `state_forwarder` after `Tv1.UpdateConfig` writes —
    /// values come from `vendor_config`, not live state, so a
    /// `const`-style signal would be a lie when the user edits.
    ///
    /// [`TvConnector::display_config`]: tisvlay_core::TvConnector::display_config
    #[zbus(property)]
    fn vendor_config(&self) -> Vec<(String, String)> {
        self.vendor_config.clone()
    }

    /// Backend-discovered diagnostic rows as ordered
    /// `(label, value)` pairs. The GUI's INFO group renders these
    /// alongside cross-vendor identity columns (TV ID, hardware,
    /// DRM connector). LG: WebSocket scheme. Future backends:
    /// whatever `Diagnostic`-visibility fields they declare. Not
    /// editable — backends refresh diagnostics during pairing
    /// only, so the user re-pairs to update.
    ///
    /// Set at interface-registration time and refreshed via
    /// `state_forwarder` after `Tv1.UpdateConfig` writes (in case
    /// a vendor field's display projection depends on others).
    #[zbus(property)]
    fn vendor_info(&self) -> Vec<(String, String)> {
        self.vendor_info.clone()
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
    /// Reconnect to the TV by closing and reopening the underlying
    /// vendor connection. The D-Bus object path stays registered
    /// throughout — only the connection changes — so no
    /// `register_tv_interfaces` / `unregister_tv_interfaces` calls
    /// are needed here. In-flight `Tv1.*` calls against this path
    /// during reconnect participate in the same per-TV `Mutex<ManagedTv>`
    /// drain semantics that `Tv1.Remove` and `apply_reload_diff`
    /// document (see `tv_manager::remove_tv`).
    async fn reconnect(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: Reconnect");
        self.manager
            .reconnect(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Re-pair the TV: run the vendor backend's pairing handshake
    /// against the TV's existing `vendor_config`, persist the refreshed
    /// credentials in place, and reconnect. Top-level fields (`name`,
    /// `profile`, `managed`, `primary_input`) stay intact.
    ///
    /// Used by clients to recover from expired credentials (LG WebOS
    /// client-key revocation, future Samsung token expiry, etc.)
    /// without forcing the user to `Tv1.Remove` + `Manager1.AddTv`
    /// from scratch.
    ///
    /// RPC returns immediately after sync validation (TV exists,
    /// `pairing_in_flight` slot available); the handshake (TV wake +
    /// SSAP TLS + user accept on TV, 10–30s typical) runs in a
    /// daemon-side spawned task. Outcome surfaces via
    /// `Manager1.PairingProgress` filtered on this TV's `tv_id` to a
    /// terminal `complete` / `failed` phase (same channel `AddTv`'s
    /// flow uses). Cancel an in-flight re-pair via
    /// `Manager1.CancelPair(tv_id)`.
    ///
    /// Concurrent re-pair attempts against the same TV are rejected
    /// with `PairingFailed("already pairing")` per the
    /// `pairing_in_flight` gate on `ManagedTv`.
    async fn re_pair(&self) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: RePair");
        self.manager
            .re_pair_tv(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Remove this TV from the daemon's configuration.
    ///
    /// Drops the in-memory TV map entry, deletes the entry from the
    /// on-disk config file, and unregisters every `Tv1.*` interface
    /// at this object path so `ObjectManager.InterfacesRemoved`
    /// fires for clients watching the surface.
    ///
    /// Lives on `Tv1` rather than `Manager1` so the handler has
    /// `&self.tv_id` directly — no `object_path → tv_id` decoding,
    /// no path-component sanitization to undo. zbus keeps `self`
    /// alive for the duration of the call even though the
    /// interface is being unregistered as part of it.
    async fn remove(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: Remove");
        self.manager
            .remove_tv(&self.tv_id)
            .await
            .map_err(DaemonError::from)?;

        let path = tv_path(&self.tv_id);
        if let Err(e) = unregister_tv_interfaces(connection, &path).await {
            warn!(tv_id = %self.tv_id, error = %e, "Failed to unregister D-Bus interfaces for TV");
        }
        Ok(())
    }

    /// Return the current TV config as a JSON string suitable for
    /// prefilling the GUI's edit pane. Top-level fields (`name`,
    /// `managed`, `system_profile`, `primary_input`) are merged
    /// with vendor-config fields filtered to non-`Secret`
    /// visibilities. The reply mirrors the keys
    /// [`Tv1::update_config`] accepts so the GUI can round-trip
    /// the same payload shape it submits.
    async fn get_config(&self) -> Result<String, DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: GetConfig");
        self.manager
            .get_tv_config_json(&self.tv_id)
            .await
            .map_err(DaemonError::from)
    }

    /// Apply a JSON patch to this TV's config atomically.
    ///
    /// Schema-validated server-side: each vendor field passes
    /// through `PairingField::validate` (same checks as `AddTv`),
    /// `Diagnostic` / `Secret` fields are rejected as not-editable,
    /// and unknown keys are surfaced as errors rather than silently
    /// ignored. Top-level fields (`name`, `managed`,
    /// `system_profile`, `primary_input`) are accepted in the same
    /// patch — atomicity covers in-memory + disk persistence.
    ///
    /// Vendor field changes drop the live connection and trigger a
    /// reconnect with the new parameters. The D-Bus object path
    /// stays registered throughout — the `Tv1.*` interface set
    /// doesn't depend on which vendor is active (capability
    /// differences surface via the `Capabilities` property, not the
    /// interface schema), so no `register_tv_interfaces` /
    /// `unregister_tv_interfaces` call is needed. In-flight `Tv1.*`
    /// calls during the reconnect participate in the same per-TV
    /// `Mutex<ManagedTv>` drain semantics documented at
    /// `tv_manager::remove_tv`.
    async fn update_config(&self, json_patch: &str) -> Result<(), DaemonError> {
        info!(tv_id = %self.tv_id, "D-Bus: UpdateConfig");
        self.manager
            .update_tv_config(&self.tv_id, json_patch)
            .await
            .map_err(DaemonError::from)
    }

    /// Get the fully resolved system profile for this TV as JSON.
    ///
    /// Returns the profile with all inheritance applied (per-TV named
    /// profile → "default" → hard-coded defaults) and the action-derived
    /// reliability tier filled in for any slot that doesn't set
    /// `qos.reliability` explicitly. Each `on_*` field is a nested
    /// `{ action, qos: { reliability } }` table mirroring the TOML
    /// shape. Symmetric with `Session1.GetEffectiveUserProfile`.
    async fn get_effective_system_profile(&self) -> Result<String, DaemonError> {
        debug!(tv_id = %self.tv_id, "D-Bus: GetEffectiveSystemProfile");
        let resolved = self
            .manager
            .get_resolved_system_profile(&self.tv_id)
            .await
            .unwrap_or_default();
        let json = serde_json::json!({
            "on_startup": resolved.on_startup,
            "on_shutdown": resolved.on_shutdown,
            "on_reboot": resolved.on_reboot,
            "on_no_session": resolved.on_no_session,
            "on_sleep": resolved.on_sleep,
            "on_wake": resolved.on_wake,
            "automate_when": resolved.automate_when.to_string(),
        });
        Ok(json.to_string())
    }

    /// Dispatch a lifecycle action on this TV with the given QoS tier.
    ///
    /// Single entry point for the lifecycle action surface. Direct
    /// callers (tray, GUI, CLI) reach this via `DaemonClient` Rust
    /// wrappers (`screen_on`, `power_on`, …) that default to
    /// best-effort QoS. Session events also flow through here,
    /// passing the resolved per-event reliability tier from the
    /// user's profile.
    ///
    /// `action` is the snake-case wire form of [`SessionAction`]
    /// (`"screen_on"`, `"screen_off"`, `"power_on"`, `"power_off"`,
    /// `"none"`). `qos_json` is a JSON object with the `QosConfig`
    /// shape; unset fields default to the action-derived asymmetric
    /// default (wake-side actions → reliable; sleep-side → best-effort).
    async fn dispatch(&self, action: &str, qos_json: &str) -> Result<(), DaemonError> {
        debug!(tv_id = %self.tv_id, action, qos_json, "D-Bus: Dispatch");
        let action = SessionAction::from_str(action)
            .map_err(|e| DaemonError::OperationFailed(format!("invalid action: {e}")))?;
        let qos_config: QosConfig = serde_json::from_str(qos_json)
            .map_err(|e| DaemonError::OperationFailed(format!("invalid qos JSON: {e}")))?;
        let reliability = qos_config
            .reliability
            .unwrap_or_else(|| default_reliability_for(action));
        let qos = tisvlay_core::ResolvedQos { reliability };
        self.manager
            .dispatch_action(&self.tv_id, action, qos)
            .await
            .map_err(DaemonError::from)
    }
}

// =============================================================================
// Power control interface
// =============================================================================

/// Power control interface.
///
/// State-changing dispatch (power on / off) moved to `Tv1.Dispatch`
/// (see `TvService` above) so it can carry a QoS tier. This interface
/// now exposes only the read-only `PowerState` / `SupportsWake`
/// properties and the `PowerCycleProgress` signal — no `&self`-anchored
/// state beyond the cached field values is needed.
#[derive(Debug)]
pub struct TvPowerService {
    /// Cached power state, kept in sync by `state_forwarder`.
    pub(crate) power_state: String,
    /// Whether the vendor backend can wake the TV from standby
    /// (e.g. LG: has a MAC for WoL). GUIs use this to disable the
    /// Power-on dispatch when both `Connected == false` and
    /// `SupportsWake == false` — the only state where wake has no
    /// path to succeed.
    pub(crate) supports_wake: bool,
}

#[interface(name = "org.tisvlay.Tv1.Power")]
impl TvPowerService {
    /// Get current power state.
    ///
    /// State-changing dispatch (power on / off) now goes through
    /// `Tv1.Dispatch` so it can carry a QoS tier — see that method's
    /// docstring on `TvService` above. The property and the
    /// `PowerCycleProgress` signal stay here.
    #[zbus(property)]
    fn power_state(&self) -> &str {
        &self.power_state
    }

    /// Whether the vendor backend can wake the TV from standby.
    /// Reflects whether `power_on()` has a viable path when the TV
    /// is currently offline; GUIs gate the Power-on button on
    /// `Connected || SupportsWake`.
    #[zbus(property)]
    fn supports_wake(&self) -> bool {
        self.supports_wake
    }

    /// Phase transitions during a power-on flow.
    ///
    /// Emitted as the manager walks through either the wake-on-LAN
    /// path (`wol_sent` → `connecting` → `connected` / `failed`) or
    /// the direct-connect path when the TV is already reachable
    /// (`direct_connect` → `connecting` → `connected` / `failed`).
    /// GUIs subscribe to render staged feedback during the 5-30 s
    /// wake window or the much-shorter direct-connect window.
    /// See [`tisvlay_core::PowerCyclePhase`] for the typed shape.
    #[zbus(signal)]
    pub async fn power_cycle_progress(emitter: &SignalEmitter<'_>, phase: &str)
    -> zbus::Result<()>;
}

// =============================================================================
// Screen control interface
// =============================================================================

/// Screen control interface (OLED-safe blanking).
///
/// `IsScreenOn` is a read-only boolean property reflecting the TV's
/// current screen state, populated by `state_forwarder` from backend
/// subscription pushes. State-changing dispatch (screen on / off) moved
/// to `Tv1.Dispatch` (see `TvService` above) so it can carry a QoS tier.
#[derive(Debug)]
pub struct TvScreenService {
    /// Cached screen state, kept in sync by `state_forwarder`.
    pub(crate) is_screen_on: bool,
}

#[interface(name = "org.tisvlay.Tv1.Screen")]
impl TvScreenService {
    /// Whether the screen is currently on.
    ///
    /// State-changing dispatch (screen on / off) now goes through
    /// `Tv1.Dispatch` so it can carry a QoS tier — see that method's
    /// docstring on `TvService`. This interface keeps the read-only
    /// property for clients that just want the current state.
    #[zbus(property)]
    fn is_screen_on(&self) -> bool {
        self.is_screen_on
    }
}

// =============================================================================
// Volume control interface
// =============================================================================

/// Volume control interface.
///
/// `Volume` and `Muted` are read-only properties reflecting the TV's
/// current state, populated by `state_forwarder` from backend
/// subscription pushes. Writes go through the `SetVolume(y)` and
/// `SetMuted(b)` methods.
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
    async fn set_volume(&self, level: u8) -> zbus::fdo::Result<()> {
        if level > 100 {
            return Err(zbus::fdo::Error::InvalidArgs("Volume must be 0-100".into()));
        }
        info!(tv_id = %self.tv_id, volume = level, "D-Bus: SetVolume");
        self.manager
            .set_volume(&self.tv_id, level)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// Whether audio is currently muted.
    #[zbus(property)]
    fn muted(&self) -> bool {
        self.muted
    }

    /// Set mute state.
    async fn set_muted(&self, muted: bool) -> zbus::fdo::Result<()> {
        info!(tv_id = %self.tv_id, muted = muted, "D-Bus: SetMuted");
        self.manager
            .set_mute(&self.tv_id, muted)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
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
/// `Backlight` is a read-only property reflecting the TV's current
/// pixel-brightness level, populated by `state_forwarder` from
/// backend subscription pushes. Writes go through the
/// `SetBacklight(y)` method.
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
    async fn set_backlight(&self, level: u8) -> zbus::fdo::Result<()> {
        if level > 100 {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Backlight must be 0-100".into(),
            ));
        }
        info!(tv_id = %self.tv_id, backlight = level, "D-Bus: SetBacklight");
        self.manager
            .set_backlight(&self.tv_id, level)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
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
    let (name, vendor, managed) = manager
        .get_tv_info(&tv_id)
        .await
        .map(|info| (info.name, info.vendor, info.managed))
        .unwrap_or_else(|| (String::new(), String::new(), true));
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

    let vendor_config = manager.vendor_config(&tv_id).await;
    let vendor_info = manager.vendor_diagnostics(&tv_id).await;

    object_server
        .at(
            path,
            TvService {
                tv_id: tv_id.clone(),
                manager: manager.clone(),
                name,
                vendor,
                managed,
                system_profile,
                primary_input,
                automate_when,
                vendor_config,
                vendor_info,
                connected,
                current_input: current_input.clone(),
            },
        )
        .await?;

    let supports_wake = manager.supports_wake(&tv_id).await;

    object_server
        .at(
            path,
            TvPowerService {
                power_state,
                supports_wake,
            },
        )
        .await?;

    object_server
        .at(path, TvScreenService { is_screen_on })
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
