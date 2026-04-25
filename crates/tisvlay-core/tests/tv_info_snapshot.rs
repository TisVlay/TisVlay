//! Snapshot tests for `TvInfo` JSON serialization.
//!
//! `TvInfo` is the canonical TV state struct returned by `DaemonClient::list_tvs()`
//! and exposed to external consumers via the CLI's `--json` output. Its wire
//! format is effectively a stability contract: any unintended change to field
//! names, ordering, or defaults would break downstream consumers (scripts,
//! GUIs, Nushell pipelines).
//!
//! These tests use `insta` snapshots — the expected output lives in
//! `tests/snapshots/` next to this file and is reviewed via `cargo insta
//! review` (or `just test-snapshot-review`).
//!
//! Run with:
//! ```sh
//! cargo test -p tisvlay-core --features dbus --test tv_info_snapshot
//! ```

#![cfg(feature = "dbus")]

use tisvlay_core::{DesktopMapping, HardwareId, TvInfo};

/// A minimal TvInfo for a connected, managed TV with a few inputs.
fn sample_connected() -> TvInfo {
    TvInfo {
        id: "living_room".to_string(),
        name: "Living Room G4".to_string(),
        address: "192.168.1.100".to_string(),
        vendor: "lg".to_string(),
        connected: true,
        managed: true,
        automated: true,
        system_profile: "default".to_string(),
        hardware_id: HardwareId::new(Some("GSM"), Some("OLED65G4SUB"), Some("serial-1")),
        desktop: DesktopMapping {
            edid: "abc123".to_string(),
            connector: "HDMI-A-2".to_string(),
            sink: "alsa_output.hdmi".to_string(),
        },
        path: "/org/tisvlay/Tv/living_room".to_string(),
        power_state: "active".to_string(),
        is_screen_on: true,
        volume: 30,
        muted: false,
        current_input: "HDMI_1".to_string(),
        backlight: 80,
        inputs: vec![
            ("HDMI_1".to_string(), "HDMI 1".to_string()),
            ("HDMI_2".to_string(), "HDMI 2".to_string()),
            ("HDMI_3".to_string(), "HDMI 3".to_string()),
        ],
    }
}

/// A disconnected, unmanaged TV — state fields have default values.
fn sample_disconnected() -> TvInfo {
    TvInfo {
        id: "guest_room".to_string(),
        name: "Guest Room".to_string(),
        address: "192.168.1.101".to_string(),
        vendor: "lg".to_string(),
        connected: false,
        managed: false,
        automated: false,
        system_profile: "guest".to_string(),
        hardware_id: HardwareId::default(),
        desktop: DesktopMapping::default(),
        path: "/org/tisvlay/Tv/guest_room".to_string(),
        power_state: "unknown".to_string(),
        is_screen_on: true,
        volume: 0,
        muted: false,
        current_input: String::new(),
        backlight: 0,
        inputs: vec![],
    }
}

#[test]
fn connected_tv_json_shape() {
    insta::assert_json_snapshot!(sample_connected());
}

#[test]
fn disconnected_tv_json_shape() {
    insta::assert_json_snapshot!(sample_disconnected());
}

#[test]
fn tv_list_json_shape() {
    // The list endpoint (`tisvlay --json list`) returns an array of TvInfo.
    // Snapshot the exact shape so downstream consumers have a stable contract.
    insta::assert_json_snapshot!(vec![sample_connected(), sample_disconnected()]);
}
