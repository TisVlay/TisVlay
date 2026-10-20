//! TV state model.

use crate::HardwareId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Current state of a TV.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
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
///
/// `#[non_exhaustive]` because future vendor backends (Samsung's
/// `NetworkStandby`, Sony's variants, …) will likely add their own
/// substates. Downstream exhaustive `match` arms must include a
/// `_` wildcard and decide a fall-through semantic — for the
/// daemon's reliable-dispatch path the safe default is "treat as
/// wake-preflight needed" so an unknown substate doesn't
/// accidentally short-circuit the wake.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
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
    /// TV is in screen saver mode — panel is on and the TV is
    /// fully usable; this variant is in the wake-permissive
    /// whitelist alongside [`Self::Active`] for that reason.
    ///
    /// Vendor backends that have a substate semantically meaning
    /// "panel dim / ambient mode / not-fully-usable" should NOT
    /// map onto `ScreenSaver` — they need their own variant (and
    /// a place in the wake-preflight set on the daemon side), or
    /// the daemon's `screen_on` short-circuit will skip the wake
    /// they actually need. The current LG mapping uses the SSAP
    /// `"screen saver"` / `"screensaver"` strings only.
    ScreenSaver,
    /// TV is in screen off mode (OLED blank).
    ScreenOff,
    /// Projector-class transient: the light source is warming up after
    /// a power-on and no usable image is projected yet. Counts as
    /// powering on (`is_on = true`) but not yet usable
    /// (`is_screen_on = false`). Instant-power backends (LG) never
    /// report this.
    WarmingUp,
    /// Projector-class transient: the light source is cooling down
    /// after a power-off. Counts as powering off (`is_on = false`,
    /// `is_screen_on = false`). Instant-power backends never report
    /// this.
    CoolingDown,
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
            "warming up" | "warmingup" => Self::WarmingUp,
            "cooling down" | "coolingdown" => Self::CoolingDown,
            _ => Self::Unknown,
        }
    }

    /// Whether the TV is considered "on" (not in standby).
    pub fn is_on(self) -> bool {
        matches!(
            self,
            Self::Active
                | Self::ActiveStandby
                | Self::ScreenSaver
                | Self::ScreenOff
                | Self::WarmingUp
        )
    }

    /// Whether the TV is awake: usable without a wake, so a client's
    /// power switch reads on. Narrower than [`Self::is_on`], which keeps
    /// `ActiveStandby` on because its NIC is alive; but that is LG's
    /// dark substate after a remote power-off, and a switch keyed on
    /// `is_on` would offer power-off for a TV the user can see is off.
    ///
    /// `ScreenOff` is awake: a lock-screen blank leaves the cache there,
    /// and a plain `screen_on` lights it. The whitelist form keeps
    /// `Unknown` and future `#[non_exhaustive]` variants on the safe
    /// fall-through the enum docs prescribe: not awake, so they wake.
    pub fn is_awake(self) -> bool {
        matches!(self, Self::Active | Self::ScreenSaver | Self::ScreenOff)
    }

    /// Whether a `screen_on` must ride the power path first: in every
    /// state that isn't [awake](Self::is_awake), screen commands stall
    /// or false-ACK (`ActiveStandby` acknowledges without ever lighting
    /// the panel), so the reliable-dispatch tier wakes the TV instead
    /// of retrying a command it cannot win. `ScreenOff` is deliberately
    /// *not* wake-needy: waking would be pure added latency.
    pub fn needs_wake_preflight(self) -> bool {
        !self.is_awake()
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
    /// `is_screen_on` is derived as a positive whitelist (`Active` /
    /// `ScreenSaver`) rather than a negation against the screen-off
    /// list. The whitelist form keeps unknown future `#[non_exhaustive]`
    /// variants on the safe default — `is_screen_on = false` — so a
    /// vendor backend adding a substate we haven't audited can't
    /// silently report the panel as lit when the daemon doesn't
    /// know. `ActiveStandby` (the LG substate after a remote-press
    /// power-off — NIC alive, panel dark) was the original
    /// motivating case; see "Bug A" in `docs/lg/firmware-quirks.md`.
    pub fn update_power(&mut self, state: PowerState) {
        self.power_state = state;
        self.is_on = state.is_on();
        self.is_screen_on = matches!(state, PowerState::Active | PowerState::ScreenSaver);
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
    /// step in the daemon (the daemon's `discover` / `discover-start`
    /// rpc handlers fill `vendor_data["mac"]` from the kernel neighbor
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
    #[must_use]
    pub fn with_vendor_data(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vendor_data.insert(key.into(), value.into());
        self
    }

    /// Attach a hardware identity to this discovery entry.
    #[must_use]
    pub fn with_hardware_id(mut self, hardware_id: HardwareId) -> Self {
        self.hardware_id = Some(hardware_id);
        self
    }

    /// Set the location URL.
    #[must_use]
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
    /// pushes `PowerState::ActiveStandby`. The daemon publishes the raw
    /// `power_state` enum on the `state/power` payload alongside the
    /// derived bools, and `is_screen_on = false` is what keeps the
    /// substate honest for the reliable-dispatch tier's verify check,
    /// which would otherwise think `screen_on` succeeded while the
    /// panel is dark. See `docs/lg/firmware-quirks.md` — "Bug A".
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

    /// Pin the awake set variant by variant, and the wake-preflight set
    /// as its complement: only the states where a screen command can
    /// actually land (`Active`, `ScreenSaver`, `ScreenOff`) read awake
    /// and skip the power path. `ActiveStandby` pins the remote-off LG
    /// the clients' power switches must read as off, `Unknown` the
    /// `#[non_exhaustive]` fall-through (a state the daemon can't vouch
    /// for must wake rather than retry into a void), and `ScreenOff`
    /// the lock-screen case that must *not* wake.
    #[test]
    fn awake_and_wake_preflight_sets_are_pinned() {
        for (state, awake) in [
            (PowerState::Active, true),
            (PowerState::ScreenSaver, true),
            (PowerState::ScreenOff, true),
            (PowerState::ActiveStandby, false),
            (PowerState::Standby, false),
            (PowerState::Suspend, false),
            (PowerState::WarmingUp, false),
            (PowerState::CoolingDown, false),
            (PowerState::Unknown, false),
        ] {
            assert_eq!(state.is_awake(), awake, "is_awake mismatch for {state:?}");
            assert_eq!(
                state.needs_wake_preflight(),
                !awake,
                "needs_wake_preflight mismatch for {state:?}",
            );
        }
    }

    /// Companion to [`active_standby_marks_screen_off`]: spell out the
    /// expected mapping for every `PowerState` variant so future changes
    /// have to touch this table.
    ///
    /// `Unknown` resolves to `is_screen_on = false` because
    /// `update_power` derives the bool from a positive whitelist of
    /// fully-usable substates (`Active` / `ScreenSaver`). Anything
    /// outside the whitelist — including `Unknown` and any future
    /// `#[non_exhaustive]` variant — is treated as panel-off, so a
    /// state the daemon can't vouch for never reads as panel-on.
    #[test]
    fn update_power_screen_on_mapping_is_complete() {
        for (state, expected_is_on, expected_screen_on) in [
            (PowerState::Active, true, true),
            (PowerState::ActiveStandby, true, false),
            (PowerState::ScreenSaver, true, true),
            (PowerState::ScreenOff, true, false),
            (PowerState::WarmingUp, true, false),
            (PowerState::CoolingDown, false, false),
            (PowerState::Standby, false, false),
            (PowerState::Suspend, false, false),
            (PowerState::Unknown, false, false),
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
