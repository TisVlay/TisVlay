//! TV state model.

use crate::HardwareId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Phases a power-on (wake-and-connect) flow walks through.
///
/// Emitted as a `(s)` D-Bus signal on `Tv1.Power.PowerCycleProgress`
/// so GUIs can show staged feedback ("Sending wake-up..." →
/// "Connecting..." → "Connected" / "Failed") instead of a single
/// 5-30 s spinner.
///
/// The manager picks one of two paths up front:
///
/// - **Direct-connect** (TV reachable already; just lost the WS):
///   skip WoL, emit `DirectConnect` → `Connecting` → `Connected`.
/// - **Wake-on-LAN** (TV in deep standby): emit `WolSent` →
///   `Connecting` → `Connected`. The backend's WoL send + connect
///   retry loop is atomic from the manager's view, so `WolSent`
///   and `Connecting` fire back-to-back just before the call.
///
/// Either path terminates in `Connected` on success or `Failed` on
/// failure. The wire string is stable; new phases get appended (not
/// renumbered) so old clients keep working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCyclePhase {
    /// Wake-on-LAN packet has been (or is about to be) sent.
    WolSent,
    /// Skipping WoL — TV reachable, going straight to connect.
    /// Mutually exclusive with `WolSent` for a given power-on flow.
    DirectConnect,
    /// Manager is attempting to establish a connection to the TV.
    Connecting,
    /// Connection succeeded; the TV is online.
    Connected,
    /// Wake-and-connect failed (TV unreachable, timeout, etc.).
    Failed,
}

impl PowerCyclePhase {
    /// Wire representation. Stable string contract for the D-Bus signal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WolSent => "wol_sent",
            Self::DirectConnect => "direct_connect",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for PowerCyclePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Current state of a TV.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TvState {
    /// Raw, vendor-reported power state. Updated by [`TvState::update_power`]
    /// alongside the derived [`Self::is_on`] / [`Self::is_screen_on`]
    /// booleans. Carries the variant distinction (e.g. `ActiveStandby`
    /// vs `ScreenOff`) that the boolean collapse loses — consumed by
    /// the daemon to gate reliable-dispatch pre-flight on the rich
    /// substate.
    #[serde(default)]
    pub power_state: PowerState,
    /// Whether the TV is powered on.
    pub is_on: bool,
    /// Whether the screen is on (false = blanked for OLED protection).
    pub is_screen_on: bool,
    /// Current volume level (0-100).
    pub volume: Option<u8>,
    /// Whether audio is muted.
    pub muted: Option<bool>,
    /// Current input ID, normalized to physical ID space (e.g., "HDMI_1").
    pub current_input: Option<String>,
    /// Current backlight level (0-100, OLED pixel brightness).
    pub backlight: Option<u8>,
    /// Current foreground app ID.
    pub current_app: Option<String>,
    /// Available inputs.
    pub inputs: Vec<InputInfo>,
    /// Vendor-internal mapping from app IDs to physical input IDs.
    ///
    /// Populated by backends that use app-based input identification
    /// (e.g., LG maps `"com.webos.app.hdmi1"` → `"HDMI_1"`). Used to
    /// normalize `current_input` to the physical ID space so it matches
    /// the `inputs` list. Skipped during serialization (internal only).
    #[serde(skip)]
    pub input_app_id_map: HashMap<String, String>,
}

/// Information about an input source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputInfo {
    /// Input ID (e.g., "HDMI_1", "HDMI_2").
    pub id: String,
    /// Display label (e.g., "HDMI 1", "PC").
    pub label: String,
    /// Whether a device is connected to this input.
    pub connected: bool,
}

/// TV power state as reported by the TV.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    /// TV is fully on.
    Active,
    /// TV is in standby.
    Standby,
    /// TV is in active standby (NIC alive, panel off — LG's substate
    /// after a remote-press power-off, before deep standby kicks in).
    ActiveStandby,
    /// TV is in suspend mode.
    Suspend,
    /// TV is in screen saver mode.
    ScreenSaver,
    /// TV is in screen off mode (OLED blank).
    ScreenOff,
    /// Unknown state.
    #[default]
    Unknown,
}

impl PowerState {
    /// Parse power state from TV response string.
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "active" => Self::Active,
            "standby" => Self::Standby,
            "suspend" => Self::Suspend,
            "active standby" | "activestandby" => Self::ActiveStandby,
            "screen saver" | "screensaver" => Self::ScreenSaver,
            "screen off" | "screenoff" => Self::ScreenOff,
            _ => Self::Unknown,
        }
    }

    /// Whether the TV is considered "on" (not in standby).
    pub fn is_on(self) -> bool {
        matches!(
            self,
            Self::Active | Self::ActiveStandby | Self::ScreenSaver | Self::ScreenOff
        )
    }
}

impl TvState {
    /// Create a new empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update state from power state response.
    ///
    /// Stores the raw [`PowerState`] in [`Self::power_state`] so
    /// downstream consumers can distinguish substates (e.g.
    /// `ActiveStandby` vs `ScreenOff` — both `is_screen_on=false`,
    /// but semantically different targets for reliable dispatch).
    ///
    /// `ActiveStandby` is in the screen-off suppression list because
    /// it's the state LG TVs enter when the user presses Power from
    /// Screen Off: NIC alive (so `is_on` stays `true` for `is_on()`),
    /// panel off. Without it the forwarder's
    /// `(is_on=true, is_screen_on=true)` → `"active"` mapping would
    /// defeat the reliable dispatch tier's verify check after a
    /// remote power-off. See "Bug A" in `docs/lg/firmware-quirks.md`.
    pub fn update_power(&mut self, state: PowerState) {
        self.power_state = state;
        self.is_on = state.is_on();
        self.is_screen_on = !matches!(
            state,
            PowerState::ScreenOff
                | PowerState::ActiveStandby
                | PowerState::Standby
                | PowerState::Suspend
        );
    }
}

/// Information about a discovered TV on the network.
///
/// This is returned by discovery mechanisms (SSDP, mDNS, etc.) and contains
/// enough information to identify and connect to the TV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredTv {
    /// Unique identifier (e.g., UUID from SSDP USN header).
    pub id: String,
    /// TV friendly name.
    pub name: String,
    /// Transport address for display (e.g., IP address, serial port, CEC address).
    pub address: String,
    /// Hardware identity (manufacturer / model / serial) as reported by
    /// the discovery protocol. Backends fill what they can read; may be
    /// `None` if the discovery channel exposes no identity data.
    #[serde(default)]
    pub hardware_id: Option<HardwareId>,
    /// Vendor identifier (e.g., "lg", "samsung").
    pub vendor: String,
    /// Location URL for device description (vendor-specific).
    #[serde(default)]
    pub location: String,
    /// Vendor-specific discovery data, free-form `key → value` map.
    ///
    /// Carries any extra fields the discovery channel was able to
    /// gather beyond the structured columns above — populated either
    /// by the vendor backend itself or by a post-discovery enrichment
    /// step in the daemon (the daemon's `Manager1.DiscoverTvs`
    /// handler fills `vendor_data["mac"]` from the kernel neighbour
    /// table after each SSDP hit, for IP-addressed backends).
    ///
    /// Consumers match these entries against the vendor's pairing
    /// schema's `autofill: "discovery.<key>"` directives — when a
    /// `PairingField` declares `autofill = "discovery.mac"` and
    /// `vendor_data["mac"]` is populated, the wizard pre-fills the
    /// field with that value.
    #[serde(default)]
    pub vendor_data: HashMap<String, String>,
}

impl DiscoveredTv {
    /// Create a new discovered TV entry.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        address: impl Into<String>,
        vendor: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            hardware_id: None,
            vendor: vendor.into(),
            location: String::new(),
            vendor_data: HashMap::new(),
        }
    }

    /// Insert a `key → value` entry into [`Self::vendor_data`].
    pub fn with_vendor_data(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vendor_data.insert(key.into(), value.into());
        self
    }

    /// Attach a hardware identity to this discovery entry.
    pub fn with_hardware_id(mut self, hardware_id: HardwareId) -> Self {
        self.hardware_id = Some(hardware_id);
        self
    }

    /// Set the location URL.
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = location.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the Action-QoS field-test failure: when an LG
    /// TV transitions from `ScreenOff` to `ActiveStandby` (user presses
    /// Power on the remote while the screen is blank), the subscription
    /// pushes `PowerState::ActiveStandby`. The forwarder derives the
    /// cached `Tv1.Power.PowerState` property from `(is_on, is_screen_on)`,
    /// so `is_screen_on` MUST be `false` for `ActiveStandby` — otherwise
    /// the cached property reads `"active"` and the reliable dispatch
    /// tier's verify check thinks `screen_on` succeeded while the panel
    /// is dark. See `docs/lg/firmware-quirks.md` — "Bug A".
    #[test]
    fn active_standby_marks_screen_off() {
        let mut state = TvState::new();
        state.update_power(PowerState::ActiveStandby);
        assert!(state.is_on, "ActiveStandby is still 'on' (NIC alive)");
        assert!(
            !state.is_screen_on,
            "ActiveStandby has the panel off — `is_screen_on` must be false \
             or the daemon's cached PowerState property would report 'active' \
             and defeat the reliable tier's verify check",
        );
    }

    /// Companion to [`active_standby_marks_screen_off`]: spell out the
    /// expected mapping for every `PowerState` variant so future changes
    /// have to touch this table.
    #[test]
    fn update_power_screen_on_mapping_is_complete() {
        for (state, expected_is_on, expected_screen_on) in [
            (PowerState::Active, true, true),
            (PowerState::ActiveStandby, true, false),
            (PowerState::ScreenSaver, true, true),
            (PowerState::ScreenOff, true, false),
            (PowerState::Standby, false, false),
            (PowerState::Suspend, false, false),
            (PowerState::Unknown, false, true),
        ] {
            let mut s = TvState::new();
            s.update_power(state);
            assert_eq!(
                s.power_state, state,
                "power_state mismatch for {state:?}: expected {state:?}, got {:?}",
                s.power_state,
            );
            assert_eq!(
                s.is_on, expected_is_on,
                "is_on mismatch for {state:?}: expected {expected_is_on}, got {}",
                s.is_on
            );
            assert_eq!(
                s.is_screen_on, expected_screen_on,
                "is_screen_on mismatch for {state:?}: expected {expected_screen_on}, got {}",
                s.is_screen_on
            );
        }
    }
}
