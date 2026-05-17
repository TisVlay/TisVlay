//! Fetches TV state from the daemon and maps it into the Slint
//! `TvViewModel` struct generated from `ui/main.slint`.
//!
//! The fetched data lives in plain `Vec`-based structs (`TvViewRaw`)
//! so it crosses the tokio→Slint thread boundary as `Send`. The Slint
//! event-loop closure then wraps the inner per-row lists into
//! `ModelRc` (which holds `Rc`, `!Send`).
//!
//! [`fetch_state`] does the IO (daemon + session bus calls) and hands
//! a [`SnapshotInputs`] off to the pure [`convert_snapshot`] function.
//! Tests construct `SnapshotInputs` directly and assert on the
//! resulting `StateSnapshot` without touching D-Bus.

use std::collections::HashMap;

use anyhow::Result;
use slint::SharedString;
use tisvlay_core::{DaemonClient, SessionClient, TvInfo};
use tracing::debug;

/// Per-TV view-model row in a thread-safe shape (no `Rc` inside).
/// Converted to the Slint-generated `TvViewModel` inside the event
/// loop, where `ModelRc::new` is allowed.
pub struct TvViewRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub vendor: SharedString,
    /// Backend's primary diagnostic descriptor — first non-Secret entry
    /// of `Tv1.VendorInfo`. LG: IP address. Future Bluetooth: MAC.
    /// Future USB-CEC: serial-port path. Rendered as the small
    /// monospace hint beside the TV name in the master list.
    pub primary_descriptor: SharedString,
    pub connected: bool,
    pub power_state: SharedString,
    pub supports_wake: bool,
    pub is_screen_on: bool,
    pub volume: i32,
    pub muted: bool,
    pub backlight: i32,
    pub current_input: SharedString,
    pub available_input_ids: Vec<SharedString>,
    pub system_profile: SharedString,
    pub user_profile: SharedString,
    pub primary_input: SharedString,
    pub managed: bool,
    pub automated: bool,
    pub hw_manufacturer: SharedString,
    pub hw_model: SharedString,
    pub hw_serial: SharedString,
    pub desktop_connector: SharedString,
    /// User-editable vendor-config rows from `Tv1.VendorConfig`
    /// (LG: IP, MAC). Rendered in the CONFIG group's view-mode
    /// list; the edit form replaces this with `EditPane.fields`
    /// when the user clicks the pencil. Plain
    /// `Vec<(String, String)>` here so the snapshot crosses the
    /// tokio→Slint thread boundary as `Send`; the conversion to
    /// `[VendorInfoRow]` (a `ModelRc`-backed list) happens inside
    /// the event-loop closure in `main.rs`.
    pub vendor_config: Vec<(String, String)>,
    /// Backend-discovered diagnostic rows from `Tv1.VendorInfo`
    /// (LG: WebSocket scheme). Read-only; rendered in the INFO
    /// group alongside hardware identity.
    pub vendor_info: Vec<(String, String)>,
}

/// Snapshot of fetched state for one refresh pass.
pub struct StateSnapshot {
    pub tvs: Vec<TvViewRaw>,
    pub system_profiles: Vec<SharedString>,
    pub user_profiles: Vec<SharedString>,
}

/// Pure-data inputs the renderer needs from one IO pass. Decoupled
/// from D-Bus so [`convert_snapshot`] can be exercised by unit tests.
pub struct SnapshotInputs {
    pub tvs: Vec<TvInfo>,
    /// `(uuid, name)` pairs from walking
    /// `ObjectManager.GetManagedObjects` on the system bus for
    /// `SystemProfile1` entries.
    pub system_profiles: Vec<(String, String)>,
    /// `(uuid, name)` pairs from walking
    /// `ObjectManager.GetManagedObjects` on the user bus for
    /// `UserProfile1` entries. Empty when the session bus isn't
    /// available.
    pub user_profiles: Vec<(String, String)>,
    /// Per-TV user-profile assignment, keyed by TV UUID. Missing
    /// entries (no session, fetch failed, or no assignment) surface
    /// as `""` in the resulting `TvViewRaw.user_profile`.
    pub user_profile_by_uuid: HashMap<String, String>,
}

/// Fetch every configured TV's state plus session-side user-profile
/// assignment + global profile lists for ComboBox population.
///
/// Single round-trip on the system bus (`GetManagedObjects` via
/// `DaemonClient::list_tvs`), one daemon call for system profiles,
/// one optional session-bus call for user profiles, and one optional
/// session-bus call per TV for the user-profile assignment.
pub async fn fetch_state(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> Result<StateSnapshot> {
    let tvs = daemon.list_tvs().await?;
    debug!(count = tvs.len(), "Fetched TVs from daemon");

    // Walk ObjectManager on both buses for the per-profile interfaces.
    // The TVs pane still keys off the friendly name for combobox
    // labels; full UUID-aware re-wiring lands with the Profiles pane
    // in Commit 6.
    let system_profiles = daemon
        .list_system_profiles_with_ids()
        .await
        .unwrap_or_default();

    let user_profiles = match session {
        Some(s) => s.list_user_profiles_with_ids().await.unwrap_or_default(),
        None => Vec::new(),
    };

    let mut user_profile_by_uuid = HashMap::new();
    if let Some(s) = session {
        for tv in &tvs {
            if let Ok(profile) = s.get_tv_user_profile(&tv.uuid).await {
                user_profile_by_uuid.insert(tv.uuid.clone(), profile);
            }
        }
    }

    Ok(convert_snapshot(SnapshotInputs {
        tvs,
        system_profiles,
        user_profiles,
        user_profile_by_uuid,
    }))
}

/// Pure renderer: shape `SnapshotInputs` (per-TV info + profile
/// lists + per-TV user-profile assignments) into the `StateSnapshot`
/// the GUI consumes. No IO; testable without an event loop.
pub fn convert_snapshot(inputs: SnapshotInputs) -> StateSnapshot {
    let SnapshotInputs {
        tvs,
        system_profiles,
        user_profiles,
        user_profile_by_uuid,
    } = inputs;

    let mut models = Vec::with_capacity(tvs.len());
    for tv in tvs {
        let user_profile = user_profile_by_uuid
            .get(&tv.uuid)
            .cloned()
            .unwrap_or_default();

        let available_input_ids: Vec<SharedString> = tv
            .inputs
            .iter()
            .map(|(id, _)| SharedString::from(id.as_str()))
            .collect();

        // Spatial hint shown alongside the name — backend's primary
        // editable descriptor (LG: IP; future Bluetooth: MAC; etc.).
        // First entry of `Tv1.VendorConfig` by convention. (Falls
        // back to the read-only diagnostic list if a backend has no
        // `UserInput` schema fields — unlikely but kept the legacy
        // behavior intact for paranoia.)
        let primary_descriptor = tv
            .vendor_config
            .first()
            .or_else(|| tv.vendor_info.first())
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        models.push(TvViewRaw {
            uuid: SharedString::from(tv.uuid),
            name: SharedString::from(tv.name),
            vendor: SharedString::from(tv.vendor),
            primary_descriptor: SharedString::from(primary_descriptor),
            connected: tv.connected,
            power_state: SharedString::from(tv.power_state),
            supports_wake: tv.supports_wake,
            is_screen_on: tv.is_screen_on,
            volume: tv.volume as i32,
            muted: tv.muted,
            backlight: tv.backlight as i32,
            current_input: SharedString::from(tv.current_input),
            available_input_ids,
            system_profile: SharedString::from(tv.system_profile),
            user_profile: SharedString::from(user_profile),
            primary_input: SharedString::from(tv.primary_input),
            managed: tv.managed,
            automated: tv.automated,
            hw_manufacturer: SharedString::from(tv.hardware_id.manufacturer.unwrap_or_default()),
            hw_model: SharedString::from(tv.hardware_id.model.unwrap_or_default()),
            hw_serial: SharedString::from(tv.hardware_id.serial.unwrap_or_default()),
            desktop_connector: SharedString::from(tv.desktop.connector),
            vendor_config: tv.vendor_config,
            vendor_info: tv.vendor_info,
        });
    }

    models.sort_by(|a, b| a.name.cmp(&b.name));

    // ObjectManager-derived rows arrive as `(uuid, name)` pairs;
    // pick the name for the combobox label until the Profiles pane
    // rebinds the editor to per-profile object paths in Commit 6.
    let system_profiles = system_profiles
        .into_iter()
        .map(|(_uuid, name)| SharedString::from(name))
        .collect();
    let user_profiles = user_profiles
        .into_iter()
        .map(|(_uuid, name)| SharedString::from(name))
        .collect();

    StateSnapshot {
        tvs: models,
        system_profiles,
        user_profiles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::{DesktopMapping, HardwareId};

    /// Build a `TvInfo` with sensible defaults so each test only
    /// names the fields it cares about. `TvInfo` doesn't derive
    /// `Default` upstream (its fields would all be valid as defaults
    /// but the type's home crate hasn't opted in).
    fn sample_tv(uuid: &str, name: &str) -> TvInfo {
        TvInfo {
            uuid: uuid.into(),
            name: name.into(),
            vendor: "lg".into(),
            connected: false,
            managed: false,
            automated: false,
            system_profile: String::new(),
            primary_input: String::new(),
            hardware_id: HardwareId::default(),
            vendor_config: Vec::new(),
            vendor_info: Vec::new(),
            desktop: DesktopMapping::default(),
            path: String::new(),
            power_state: "unknown".into(),
            supports_wake: false,
            is_screen_on: false,
            volume: 0,
            muted: false,
            current_input: String::new(),
            backlight: 0,
            inputs: Vec::new(),
        }
    }

    fn empty_inputs() -> SnapshotInputs {
        SnapshotInputs {
            tvs: Vec::new(),
            system_profiles: Vec::new(),
            user_profiles: Vec::new(),
            user_profile_by_uuid: HashMap::new(),
        }
    }

    #[test]
    fn empty_inputs_produce_empty_snapshot() {
        let snap = convert_snapshot(empty_inputs());
        assert!(snap.tvs.is_empty());
        assert!(snap.system_profiles.is_empty());
        assert!(snap.user_profiles.is_empty());
    }

    #[test]
    fn single_tv_round_trips_through_conversion() {
        let mut tv = sample_tv("uuid-1", "Living Room");
        tv.connected = true;
        tv.power_state = "active".into();
        tv.volume = 42;
        tv.backlight = 80;
        tv.muted = true;
        tv.is_screen_on = true;
        tv.current_input = "HDMI_2".into();
        tv.inputs = vec![
            ("HDMI_1".into(), "HDMI 1".into()),
            ("HDMI_2".into(), "HDMI 2".into()),
        ];
        tv.system_profile = "default".into();
        tv.hardware_id = HardwareId {
            manufacturer: Some("GSM".into()),
            model: Some("OLED65G4SUB".into()),
            serial: Some("XYZ123".into()),
        };

        let inputs = SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);

        assert_eq!(snap.tvs.len(), 1);
        let row = &snap.tvs[0];
        assert_eq!(row.uuid.as_str(), "uuid-1");
        assert_eq!(row.name.as_str(), "Living Room");
        assert!(row.connected);
        assert_eq!(row.power_state.as_str(), "active");
        assert_eq!(row.volume, 42);
        assert_eq!(row.backlight, 80);
        assert!(row.muted);
        assert!(row.is_screen_on);
        assert_eq!(row.current_input.as_str(), "HDMI_2");
        assert_eq!(
            row.available_input_ids,
            vec![SharedString::from("HDMI_1"), SharedString::from("HDMI_2")]
        );
        assert_eq!(row.system_profile.as_str(), "default");
        assert_eq!(row.hw_manufacturer.as_str(), "GSM");
        assert_eq!(row.hw_model.as_str(), "OLED65G4SUB");
        assert_eq!(row.hw_serial.as_str(), "XYZ123");
    }

    #[test]
    fn missing_hardware_id_fields_become_empty_strings() {
        let tv = sample_tv("uuid-1", "TV");
        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        let row = &snap.tvs[0];
        assert_eq!(row.hw_manufacturer.as_str(), "");
        assert_eq!(row.hw_model.as_str(), "");
        assert_eq!(row.hw_serial.as_str(), "");
    }

    #[test]
    fn primary_descriptor_prefers_vendor_config_first_entry() {
        let mut tv = sample_tv("uuid-1", "TV");
        tv.vendor_config = vec![
            ("ip".into(), "192.168.1.10".into()),
            ("mac".into(), "AA:BB:CC:DD:EE:FF".into()),
        ];
        tv.vendor_info = vec![("scheme".into(), "wss".into())];

        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        assert_eq!(snap.tvs[0].primary_descriptor.as_str(), "192.168.1.10");
    }

    #[test]
    fn primary_descriptor_falls_back_to_vendor_info_when_config_empty() {
        let mut tv = sample_tv("uuid-1", "TV");
        // vendor_config empty; vendor_info has the only candidate.
        tv.vendor_info = vec![("scheme".into(), "wss".into())];

        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        assert_eq!(snap.tvs[0].primary_descriptor.as_str(), "wss");
    }

    #[test]
    fn primary_descriptor_empty_when_both_lists_empty() {
        let tv = sample_tv("uuid-1", "TV");
        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        assert_eq!(snap.tvs[0].primary_descriptor.as_str(), "");
    }

    #[test]
    fn tvs_sort_by_name_alphabetically() {
        let inputs = SnapshotInputs {
            tvs: vec![
                sample_tv("uuid-c", "Bedroom"),
                sample_tv("uuid-a", "Office"),
                sample_tv("uuid-b", "Attic"),
            ],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let names: Vec<_> = snap.tvs.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, vec!["Attic", "Bedroom", "Office"]);
    }

    #[test]
    fn user_profile_assignment_resolves_by_uuid() {
        let inputs = SnapshotInputs {
            tvs: vec![sample_tv("uuid-a", "TV-A"), sample_tv("uuid-b", "TV-B")],
            user_profile_by_uuid: HashMap::from([
                ("uuid-a".into(), "movies".into()),
                // uuid-b deliberately absent — surfaces as "" below.
            ]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let by_uuid: HashMap<_, _> = snap
            .tvs
            .iter()
            .map(|t| (t.uuid.to_string(), t.user_profile.to_string()))
            .collect();
        assert_eq!(by_uuid["uuid-a"], "movies");
        assert_eq!(by_uuid["uuid-b"], "");
    }

    #[test]
    fn profile_lists_keep_names_from_uuid_name_pairs() {
        // The post-v2 wire shape is `(uuid_string, name)`; the Slint
        // model surface still wants just the friendly name for the
        // current combobox label.
        let inputs = SnapshotInputs {
            system_profiles: vec![
                (
                    "00000000-0000-0000-0000-000000000001".into(),
                    "default".into(),
                ),
                (
                    "a1b2c3d4-0000-0000-0000-000000000001".into(),
                    "evening".into(),
                ),
            ],
            user_profiles: vec![(
                "00000000-0000-0000-0000-000000000002".into(),
                "movies".into(),
            )],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        assert_eq!(
            snap.system_profiles,
            vec![SharedString::from("default"), SharedString::from("evening")]
        );
        assert_eq!(snap.user_profiles, vec![SharedString::from("movies")]);
    }
}
