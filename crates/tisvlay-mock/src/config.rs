//! Vendor config for the mock backend.

use serde::{Deserialize, Serialize};

use crate::pairing::PairingShorthand;

/// Mock TV configuration, stored under `[tv.mock]` in the daemon config.
///
/// Only `address` is required — everything else has sensible defaults.
/// The address is purely a label shown in logs and the tray; no actual
/// network traffic happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockVendorConfig {
    /// Virtual address for the mock TV (e.g., `"mock://living_room"`
    /// or `"192.168.255.1"`). Shown in logs and the tray tooltip.
    #[serde(default = "default_address")]
    pub address: String,

    /// Initial power state: one of `"active"`, `"standby"`, `"screen_off"`.
    #[serde(default = "default_power_state")]
    pub initial_power_state: String,

    /// Initial volume (0-100).
    #[serde(default = "default_volume")]
    pub initial_volume: u8,

    /// Initial input ID (must exist in `inputs`).
    #[serde(default = "default_input")]
    pub initial_input: String,

    /// Initial backlight level (0-100).
    #[serde(default = "default_backlight")]
    pub initial_backlight: u8,

    /// Available inputs as `(id, label)` pairs.
    #[serde(default = "default_inputs")]
    pub inputs: Vec<(String, String)>,

    /// Test knob: every `connect` fails with a backend error.
    /// Exercises dial-failure paths (pairing rollback, retry/backoff)
    /// without a real unreachable device.
    #[serde(default)]
    pub refuse_connect: bool,

    /// Test knob: every `connect` fails as if the local device node
    /// couldn't be opened (`Error::DeviceUnavailable`), the failure a
    /// local-device TV keeps retrying past its budget.
    #[serde(default)]
    pub device_unavailable: bool,

    /// Whether this mock claims wake support. Defaults on; tests flip
    /// it through update-config to exercise transport-fact refreshes.
    #[serde(default = "default_wake")]
    pub wake: bool,

    /// Wake-on-LAN MAC, mirroring a real vendor's field so the daemon's
    /// generic MAC handling (pair-time fill, display rows) has something
    /// to act on. The mock's wake stays a no-op either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,

    /// Test knob: the link MAC `get_link_mac` reports, as if the device
    /// said so. `None` reports nothing, like a backend that can't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_mac: Option<String>,

    /// Pairing behavior shorthand.
    ///
    /// - `"manual"` (default): pairing waits for an explicit
    ///   `MockHandle::simulate_user_*` call from test code
    /// - `"accept"`: auto-accept on `start_pairing()`
    /// - `"reject"`: auto-reject on `start_pairing()`
    /// - `"timeout"`: simulate a timeout on `start_pairing()`
    ///
    /// Tests that set an outcome via the handle take precedence over
    /// this shorthand: the handle's value is consumed first, and the
    /// shorthand is only applied if no handle outcome is present.
    #[serde(default)]
    pub pairing: PairingShorthand,

    /// If non-empty, `set_backlight(target)` pushes each of these
    /// values via the subscription channel, then pushes `target`.
    /// Models real LG TVs that ramp backlight gradually rather than
    /// snapping, emitting multiple `picture/backlight` subscription
    /// updates per setSystemSettings call. Default empty (snap, like
    /// volume).
    #[serde(default)]
    pub backlight_ramp_intermediates: Vec<u8>,

    /// Delay between intermediate ramp pushes, in milliseconds. With
    /// `backlight_ramp_intermediates` non-empty, each step waits this
    /// long before pushing the next. Default 0 = `tokio::yield_now`
    /// (effectively instant — fine for "is the emit pipeline ordered"
    /// tests). Set to a few ms for tests that need to interleave a
    /// new command mid-ramp deterministically.
    #[serde(default)]
    pub backlight_ramp_step_delay_ms: u64,

    /// How long `power_off` takes at the TV, in milliseconds, before the
    /// standby push and the return. Default 0 (instant). For tests of
    /// what waits on a power-off landing, such as the sleep inhibitor.
    #[serde(default)]
    pub power_off_delay_ms: u64,

    /// How long an in-place `power_on` takes, in milliseconds, before it
    /// wakes the TV and returns. Default 0 (instant). For tests of what
    /// can interrupt a wake that's still running.
    #[serde(default)]
    pub power_on_delay_ms: u64,

    /// Make this mock TV behave like a poll-only backend.
    ///
    /// When `true`, the mock connection's `subscribe_state` returns
    /// `NotSupported` instead of a push channel — modeling a backend
    /// (e.g. an Epson projector over a request/response transport) that
    /// has no real-time state stream. The daemon then drives the TV
    /// through its periodic `state_poll_loop` / `get_state` path and
    /// the reliable-dispatch active-verify path instead of subscription
    /// pushes. Default `false` (push, like LG) so existing mock-based
    /// tests are unaffected.
    #[serde(default)]
    pub disable_subscription: bool,

    /// When `true`, the connector reports `reachable_in_standby` — modeling
    /// a bus-style transport (CEC) whose connection survives standby, so the
    /// daemon keeps the connection (and `Connected`) across a power-off and
    /// wakes by power state. Default `false` (network-like, drop on
    /// power-off) so existing mock-based tests are unaffected.
    #[serde(default)]
    pub reachable_in_standby: bool,

    /// When `true`, the connector reports `emulate_screen_via_power` — modeling
    /// a backend (CEC) with no discrete screen-blank, so the daemon advertises
    /// `SCREEN` and dispatches `screen_off`/`screen_on` as `power_off`/
    /// `power_on`. Default `false` so existing mock-based tests are unaffected.
    #[serde(default)]
    pub emulate_screen_via_power: bool,

    /// When set, the connector reports this path as its `local_device` —
    /// modeling a backend (CEC) whose connection needs a device node, so
    /// the daemon gates the dial on its availability. Default `None`
    /// (network-shaped) so existing mock-based tests are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_device: Option<String>,
}

fn default_wake() -> bool {
    true
}

fn default_address() -> String {
    "mock://tv".to_string()
}

fn default_power_state() -> String {
    "active".to_string()
}

fn default_volume() -> u8 {
    30
}

fn default_input() -> String {
    "HDMI_1".to_string()
}

fn default_backlight() -> u8 {
    80
}

fn default_inputs() -> Vec<(String, String)> {
    vec![
        ("HDMI_1".to_string(), "HDMI 1".to_string()),
        ("HDMI_2".to_string(), "HDMI 2".to_string()),
        ("HDMI_3".to_string(), "HDMI 3".to_string()),
    ]
}

impl Default for MockVendorConfig {
    fn default() -> Self {
        Self {
            address: default_address(),
            initial_power_state: default_power_state(),
            initial_volume: default_volume(),
            initial_input: default_input(),
            initial_backlight: default_backlight(),
            inputs: default_inputs(),
            refuse_connect: false,
            device_unavailable: false,
            wake: true,
            mac: None,
            link_mac: None,
            pairing: PairingShorthand::default(),
            backlight_ramp_intermediates: Vec::new(),
            backlight_ramp_step_delay_ms: 0,
            power_off_delay_ms: 0,
            power_on_delay_ms: 0,
            disable_subscription: false,
            reachable_in_standby: false,
            emulate_screen_via_power: false,
            local_device: None,
        }
    }
}
