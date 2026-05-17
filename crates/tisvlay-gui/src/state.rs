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
use tisvlay_core::{DaemonClient, ProfileSummary, SessionClient, TvInfo};
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

/// One TV that references a profile. Carries the UUID for
/// click-through navigation (`Profiles.open-tv(uuid)`) and the
/// friendly name for chip rendering. Plain types so the snapshot
/// crosses the tokio→Slint thread boundary as `Send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TvRefRaw {
    pub uuid: SharedString,
    pub name: SharedString,
}

/// One profile-catalog entry for the Profiles pane master list. Drives
/// the section row's name + `(default)` badge + `Nx` trailing count,
/// and the detail header's "Used by" chips. Reference list is
/// resolved Rust-side by joining each profile's `ReferencedBy: as`
/// (UUIDs of referring TVs, daemon-authoritative) with the in-memory
/// TV list to recover friendly names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRowRaw {
    /// Hyphenated UUID string from `SystemProfile1.Id` /
    /// `UserProfile1.Id`. Stable identity across renames; round-trips
    /// back to Rust on `Profiles.select` / `confirm-delete`.
    pub uuid: SharedString,
    pub name: SharedString,
    /// True for the reserved default profile
    /// (`DEFAULT_SYSTEM_PROFILE_UUID` / `DEFAULT_USER_PROFILE_UUID`).
    /// Drives the `(default)` badge in the master list and disables
    /// the Delete button in the detail header.
    pub is_default: bool,
    pub referenced_by: Vec<TvRefRaw>,
    /// Comma-joined friendly names of `referenced_by`. Pre-built so
    /// the delete-confirm dialog can render an inline "used by X, Y,
    /// Z" message without a string-join expression (Slint doesn't
    /// have one).
    pub referenced_by_summary: SharedString,
}

/// Snapshot of fetched state for one refresh pass.
pub struct StateSnapshot {
    pub tvs: Vec<TvViewRaw>,
    pub system_profiles: Vec<SharedString>,
    pub user_profiles: Vec<SharedString>,
    /// Profiles-pane master-list rows for the System section.
    pub system_profile_rows: Vec<ProfileRowRaw>,
    /// Profiles-pane master-list rows for the User section. Empty
    /// when the session bus isn't reachable.
    pub user_profile_rows: Vec<ProfileRowRaw>,
}

/// Pure-data inputs the renderer needs from one IO pass. Decoupled
/// from D-Bus so [`convert_snapshot`] can be exercised by unit tests.
pub struct SnapshotInputs {
    pub tvs: Vec<TvInfo>,
    /// Full `ProfileSummary` rows from
    /// `DaemonClient::list_system_profiles_full`, carrying UUID +
    /// name + `ReferencedBy` UUIDs.
    pub system_profiles: Vec<ProfileSummary>,
    /// Full `ProfileSummary` rows from
    /// `SessionClient::list_user_profiles_full`. Empty when the
    /// session bus isn't reachable.
    pub user_profiles: Vec<ProfileSummary>,
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
    // The Profiles pane needs the full `ProfileSummary` (UUID + name
    // + `ReferencedBy`) to render the master list + detail-header
    // chips; the TVs pane still drops to the friendly-name list for
    // its profile ComboBoxes (the v2 full re-bind for those is out
    // of scope for commit 6).
    let system_profiles = daemon.list_system_profiles_full().await.unwrap_or_default();

    let user_profiles = match session {
        Some(s) => s.list_user_profiles_full().await.unwrap_or_default(),
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

    // System / User combobox labels for the TVs-pane PROFILES rows.
    // Still keyed off the friendly name; the per-field UUID rebind
    // happens once the Profiles pane editor lands (commit 7+).
    let system_profile_names = system_profiles
        .iter()
        .map(|p| SharedString::from(p.name.as_str()))
        .collect();
    let user_profile_names = user_profiles
        .iter()
        .map(|p| SharedString::from(p.name.as_str()))
        .collect();

    let system_profile_rows = build_profile_rows(
        &system_profiles,
        &models,
        tisvlay_core::DEFAULT_SYSTEM_PROFILE_UUID,
    );
    let user_profile_rows = build_profile_rows(
        &user_profiles,
        &models,
        tisvlay_core::DEFAULT_USER_PROFILE_UUID,
    );

    StateSnapshot {
        tvs: models,
        system_profiles: system_profile_names,
        user_profiles: user_profile_names,
        system_profile_rows,
        user_profile_rows,
    }
}

/// Build the Profiles pane master-list rows for one catalog. Each
/// row's `referenced_by` set comes from the profile's daemon-side
/// `ReferencedBy: as` (authoritative — accounts for inheritance
/// plus default-profile fan-out), joined with the in-memory TV
/// list to recover friendly names for chip rendering. Rows whose
/// referencing TVs aren't in the snapshot (recently-removed TV
/// racing with the refresh) still show with the UUID as the chip
/// label so the link doesn't silently disappear.
///
/// Rows arrive sorted by name with `default` pinned first, matching
/// the convention the v1 layout established and reading "stable
/// anchor on top, alphabetical custom profiles below" in the
/// master list.
fn build_profile_rows(
    summaries: &[ProfileSummary],
    tvs: &[TvViewRaw],
    default_uuid: uuid::Uuid,
) -> Vec<ProfileRowRaw> {
    let default_uuid = default_uuid.to_string();
    let mut rows: Vec<ProfileRowRaw> = summaries
        .iter()
        .map(|p| {
            let referenced_by: Vec<TvRefRaw> = p
                .referenced_by
                .iter()
                .map(|tv_uuid| {
                    let name = tvs
                        .iter()
                        .find(|t| t.uuid.as_str() == tv_uuid.as_str())
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| SharedString::from(tv_uuid.as_str()));
                    TvRefRaw {
                        uuid: SharedString::from(tv_uuid.as_str()),
                        name,
                    }
                })
                .collect();
            let summary = referenced_by
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            ProfileRowRaw {
                uuid: SharedString::from(p.uuid.as_str()),
                name: SharedString::from(p.name.as_str()),
                is_default: p.uuid == default_uuid,
                referenced_by,
                referenced_by_summary: SharedString::from(summary),
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        // Default pinned first, the rest alphabetic by name.
        match (a.is_default, b.is_default) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    rows
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

    fn summary(uuid: &str, name: &str, referenced_by: &[&str]) -> ProfileSummary {
        ProfileSummary {
            uuid: uuid.to_string(),
            name: name.to_string(),
            referenced_by: referenced_by.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn profile_lists_keep_names_from_summaries() {
        // The post-v2 wire shape is `ProfileSummary`; the TVs pane's
        // combobox model still wants just the friendly name. The
        // Profiles pane reads `system_profile_rows` / `user_profile_rows`.
        let inputs = SnapshotInputs {
            system_profiles: vec![
                summary("00000000-0000-0000-0000-000000000001", "default", &[]),
                summary("a1b2c3d4-0000-0000-0000-000000000001", "evening", &[]),
            ],
            user_profiles: vec![summary(
                "00000000-0000-0000-0000-000000000002",
                "movies",
                &[],
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

    #[test]
    fn profile_rows_default_pinned_first_then_alphabetic() {
        let inputs = SnapshotInputs {
            system_profiles: vec![
                summary("a1b2c3d4-0000-0000-0000-000000000003", "zen", &[]),
                summary("00000000-0000-0000-0000-000000000001", "default", &[]),
                summary("a1b2c3d4-0000-0000-0000-000000000002", "guest", &[]),
            ],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let names: Vec<_> = snap
            .system_profile_rows
            .iter()
            .map(|r| r.name.to_string())
            .collect();
        assert_eq!(names, vec!["default", "guest", "zen"]);
        assert!(snap.system_profile_rows[0].is_default);
        assert!(!snap.system_profile_rows[1].is_default);
    }

    #[test]
    fn profile_rows_resolve_referenced_tv_names() {
        let inputs = SnapshotInputs {
            tvs: vec![
                sample_tv("tv-uuid-1", "Living Room"),
                sample_tv("tv-uuid-2", "Bedroom"),
            ],
            system_profiles: vec![summary(
                "a1b2c3d4-0000-0000-0000-000000000001",
                "evening",
                &["tv-uuid-1", "tv-uuid-2"],
            )],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let row = snap
            .system_profile_rows
            .iter()
            .find(|r| r.name.as_str() == "evening")
            .unwrap();
        assert_eq!(row.referenced_by.len(), 2);
        let names: Vec<_> = row
            .referenced_by
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        // Names are TV-list order (alphabetic from `models.sort_by`),
        // so Bedroom before Living Room.
        assert_eq!(
            row.referenced_by_summary.as_str(),
            names.join(", ").as_str(),
        );
    }

    #[test]
    fn profile_rows_fallback_to_uuid_when_tv_missing() {
        // TV referenced in ReferencedBy that doesn't appear in the
        // snapshot (recently-removed TV racing with the refresh)
        // still renders — the chip just shows the raw UUID instead
        // of vanishing.
        let inputs = SnapshotInputs {
            system_profiles: vec![summary(
                "a1b2c3d4-0000-0000-0000-000000000001",
                "lonely",
                &["orphan-uuid"],
            )],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let row = &snap.system_profile_rows[0];
        assert_eq!(row.referenced_by.len(), 1);
        assert_eq!(row.referenced_by[0].uuid.as_str(), "orphan-uuid");
        assert_eq!(row.referenced_by[0].name.as_str(), "orphan-uuid");
    }
}
