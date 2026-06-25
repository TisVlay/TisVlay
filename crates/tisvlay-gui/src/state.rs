//! Fetches TV state from the daemon and maps it into the Slint
//! `TvViewModel` struct generated from `ui/main.slint`.
//!
//! The fetched data lives in plain `Vec`-based structs (`TvViewRaw`,
//! `ProfileRowRaw`) so it crosses the tokio→Slint thread boundary as
//! `Send`. The Slint event-loop closure then wraps the inner per-row
//! lists into `ModelRc` (which holds `Rc`, `!Send`).
//!
//! [`fetch_state`] does the IO (daemon + session bus calls) and hands
//! a [`SnapshotInputs`] off to the pure [`convert_snapshot`] function.
//! Tests construct `SnapshotInputs` directly and assert on the
//! resulting `StateSnapshot` without touching D-Bus.

use std::collections::HashMap;

use anyhow::Result;
use gettextrs::gettext;
use slint::SharedString;
use tisvlay_core::{
    Capabilities, DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID, DaemonClient,
    SessionClient, SystemProfileBody, TvAssignment, TvInfo, UserProfileBody, references_of,
    user_references_of,
};
use tracing::debug;
use uuid::Uuid;

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
    /// Per-capability flags from `Tv1.Capabilities` (mirrors
    /// [`TvInfo::capabilities`]), used to hide controls a backend can't
    /// drive — a CEC TV has no volume, mute, backlight, or screen-blank.
    /// Static per vendor, so controls don't flicker as the TV's
    /// connection cycles.
    pub has_screen: bool,
    pub has_volume: bool,
    pub has_mute: bool,
    pub has_input: bool,
    pub has_backlight: bool,
    pub is_screen_on: bool,
    pub volume: i32,
    pub muted: bool,
    pub backlight: i32,
    pub current_input: SharedString,
    pub available_input_ids: Vec<SharedString>,
    pub system_profile: SharedString,
    /// Immutable UUID of the TV's assigned system profile. Mirrors
    /// the display name above for ComboBox rendering, but feeds the
    /// PROFILES-row pencil's navigation handoff to the Profiles
    /// pane. Empty assignments collapse to `DEFAULT_SYSTEM_PROFILE_UUID`
    /// here so the Slint click handler never has to fall back from
    /// `""`.
    pub system_profile_uuid: SharedString,
    pub user_profile: SharedString,
    pub user_profile_uuid: SharedString,
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

/// One TV referencing a profile, in the thread-safe shape Rust passes
/// through to the Slint `ProfileTvRef` struct on the event-loop side.
/// `automated` mirrors the per-TV `TvInfo::automated` flag — drives
/// the Used-by row's per-TV color tint (Active/success vs
/// Assigned/info), matching the 3-state classification the master
/// list row uses for the row-level aggregate.
#[derive(Debug, Clone)]
pub struct ProfileTvRefRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub automated: bool,
}

/// One profile row for the Profiles-pane master list. Mirrors the
/// Slint-generated `ProfileRow` shape but stays `Send` (the inner
/// `Vec` becomes a `ModelRc` only inside the event-loop push).
///
/// `name` is the bare profile name as it lives on the daemon's wire
/// — used by the rename-pencil input prefill and any daemon round-
/// trip. `display_name` is the synthesized form shown in read-only
/// surfaces: localized "default" for the reserved-default UUID,
/// suffixed with the default-indicator glyph. See
/// [`display_profile_name`] for the synthesis rules.
#[derive(Debug, Clone)]
pub struct ProfileRowRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub display_name: SharedString,
    pub is_default: bool,
    pub referenced_by: Vec<ProfileTvRefRaw>,
    pub referenced_by_summary: SharedString,
    pub active: bool,
}

/// Snapshot of fetched state for one refresh pass.
pub struct StateSnapshot {
    pub tvs: Vec<TvViewRaw>,
    /// Friendly-name list for the TVs-pane System-profile ComboBox.
    /// Derived from the body catalog so the same fetch feeds both
    /// the Profiles pane (full rows) and the TVs pane (just names).
    pub system_profiles: Vec<SharedString>,
    /// User-side counterpart for the TVs-pane User-profile ComboBox.
    pub user_profiles: Vec<SharedString>,
    /// Master-list rows for the Profiles pane's System section.
    pub system_profile_rows: Vec<ProfileRowRaw>,
    /// Master-list rows for the Profiles pane's User section.
    pub user_profile_rows: Vec<ProfileRowRaw>,
}

/// Pure-data inputs the renderer needs from one IO pass. Decoupled
/// from D-Bus so [`convert_snapshot`] can be exercised by unit tests.
pub struct SnapshotInputs {
    pub tvs: Vec<TvInfo>,
    /// System-profile catalog, keyed by parsed UUID. From
    /// `Manager1.ListSystemProfiles` (property-bag wire).
    pub system_profile_bodies: HashMap<Uuid, SystemProfileBody>,
    /// User-profile catalog, keyed by parsed UUID. From
    /// `Session1.ListUserProfiles` (property-bag wire). Empty when
    /// the session bus isn't available.
    pub user_profile_bodies: HashMap<Uuid, UserProfileBody>,
    /// Per-TV user-profile assignment, keyed by TV UUID. Missing
    /// entries (no session, fetch failed, or no assignment) surface
    /// as `""` in the resulting `TvViewRaw.user_profile`.
    pub user_profile_by_uuid: HashMap<String, String>,
}

/// Fetch every configured TV's state plus session-side user-profile
/// assignment + global profile catalogs.
///
/// Single round-trip on the system bus (`GetManagedObjects` via
/// `DaemonClient::list_tvs`), one daemon call for the system-profile
/// body catalog, one optional session-bus call for the user-profile
/// body catalog, and one optional session-bus call per TV for the
/// user-profile assignment.
pub async fn fetch_state(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> Result<StateSnapshot> {
    let tvs = daemon.list_tvs().await?;
    debug!(count = tvs.len(), "Fetched TVs from daemon");

    let system_profile_bodies = daemon
        .list_system_profile_bodies()
        .await
        .unwrap_or_default();

    let user_profile_bodies = match session {
        Some(s) => s.list_user_profile_bodies().await.unwrap_or_default(),
        None => HashMap::new(),
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
        system_profile_bodies,
        user_profile_bodies,
        user_profile_by_uuid,
    }))
}

/// Fetch a single system-profile body. Wraps the typed CRUD wrapper
/// so callers can refresh one row on a `SystemProfileChanged` signal
/// without going through a full catalog list. Errors propagate so the
/// caller can decide whether to retry or drop the cached row.
pub async fn fetch_system_profile_body(
    daemon: &DaemonClient,
    uuid: Uuid,
) -> Result<SystemProfileBody> {
    daemon.get_system_profile(uuid).await
}

/// User-side mirror of [`fetch_system_profile_body`].
pub async fn fetch_user_profile_body(
    session: &SessionClient,
    uuid: Uuid,
) -> Result<UserProfileBody> {
    session.get_user_profile(uuid).await
}

/// Pure renderer: shape `SnapshotInputs` into the `StateSnapshot` the
/// GUI consumes. No IO; testable without an event loop.
pub fn convert_snapshot(inputs: SnapshotInputs) -> StateSnapshot {
    let SnapshotInputs {
        tvs,
        system_profile_bodies,
        user_profile_bodies,
        user_profile_by_uuid,
    } = inputs;

    let mut models = Vec::with_capacity(tvs.len());
    for tv in &tvs {
        let user_profile = user_profile_by_uuid
            .get(&tv.uuid)
            .cloned()
            .unwrap_or_default();

        // Pre-resolved UUIDs for the PROFILES-row pencil. Empty
        // source (no assignment) collapses to the reserved-default
        // UUID so the Slint click handler can route unconditionally
        // without a `== "" ? default` fallback.
        let system_profile_uuid = if tv.system_profile.is_empty() {
            DEFAULT_SYSTEM_PROFILE_UUID.hyphenated().to_string()
        } else {
            tv.system_profile.clone()
        };
        let user_profile_uuid = if user_profile.is_empty() {
            DEFAULT_USER_PROFILE_UUID.hyphenated().to_string()
        } else {
            user_profile.clone()
        };

        let available_input_ids: Vec<SharedString> = tv
            .inputs
            .iter()
            .map(|(id, _)| SharedString::from(id.as_str()))
            .collect();

        // Spatial hint shown alongside the name — backend's primary
        // editable descriptor (LG: IP; future Bluetooth: MAC; etc.).
        // First entry of `Tv1.VendorConfig` by convention. Falls
        // back to the read-only diagnostic list if a backend has no
        // `UserInput` schema fields.
        let primary_descriptor = tv
            .vendor_config
            .first()
            .or_else(|| tv.vendor_info.first())
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        models.push(TvViewRaw {
            uuid: SharedString::from(tv.uuid.as_str()),
            name: SharedString::from(tv.name.as_str()),
            vendor: SharedString::from(tv.vendor.as_str()),
            primary_descriptor: SharedString::from(primary_descriptor),
            connected: tv.connected,
            power_state: SharedString::from(tv.power_state.as_str()),
            supports_wake: tv.supports_wake,
            has_screen: tv.capabilities.contains(Capabilities::SCREEN),
            has_volume: tv.capabilities.contains(Capabilities::VOLUME),
            has_mute: tv.capabilities.contains(Capabilities::MUTE),
            has_input: tv.capabilities.contains(Capabilities::INPUT),
            has_backlight: tv.capabilities.contains(Capabilities::BACKLIGHT),
            is_screen_on: tv.is_screen_on,
            volume: tv.volume as i32,
            muted: tv.muted,
            backlight: tv.backlight as i32,
            current_input: SharedString::from(tv.current_input.as_str()),
            available_input_ids,
            // ComboBox renders `current-value` verbatim. Resolve the
            // wire UUID to the friendly name so the picker matches its
            // `system-profiles` / `user-profiles` model. Unknown UUIDs
            // fall through to the raw string so a stale-state drift
            // stays visible instead of blanking the cell.
            system_profile: SharedString::from(resolve_profile_name(
                &tv.system_profile,
                &system_profile_bodies,
                DEFAULT_SYSTEM_PROFILE_UUID,
            )),
            system_profile_uuid: SharedString::from(system_profile_uuid),
            user_profile: SharedString::from(resolve_profile_name(
                &user_profile,
                &user_profile_bodies,
                DEFAULT_USER_PROFILE_UUID,
            )),
            user_profile_uuid: SharedString::from(user_profile_uuid),
            primary_input: SharedString::from(tv.primary_input.as_str()),
            managed: tv.managed,
            automated: tv.automated,
            hw_manufacturer: SharedString::from(
                tv.hardware_id.manufacturer.clone().unwrap_or_default(),
            ),
            hw_model: SharedString::from(tv.hardware_id.model.clone().unwrap_or_default()),
            hw_serial: SharedString::from(tv.hardware_id.serial.clone().unwrap_or_default()),
            desktop_connector: SharedString::from(tv.desktop.connector.as_str()),
            vendor_config: tv.vendor_config.clone(),
            vendor_info: tv.vendor_info.clone(),
        });
    }

    models.sort_by(|a, b| a.name.cmp(&b.name));

    // ---- Build TvAssignment list once per render — every system /
    // user profile row reuses it via references_of() / user_references_of().
    // Per-TV automation state stays alongside the assignment so we
    // don't have to re-walk the TV list when computing `active`.
    let tv_assignments: Vec<TvAssignment> = tvs
        .iter()
        .filter_map(|tv| {
            let tv_uuid = Uuid::parse_str(&tv.uuid).ok()?;
            Some(TvAssignment {
                tv_uuid,
                system_profile: Uuid::parse_str(&tv.system_profile).ok(),
                user_profile: user_profile_by_uuid
                    .get(&tv.uuid)
                    .and_then(|s| Uuid::parse_str(s).ok()),
            })
        })
        .collect();

    // Per-TV (uuid → (display-name, automated)) lookup used to
    // resolve `referenced_by` chips and the `active` flag without
    // re-iterating the full TV list per row.
    let tv_meta: HashMap<Uuid, (String, bool)> = tvs
        .iter()
        .filter_map(|tv| {
            let parsed = Uuid::parse_str(&tv.uuid).ok()?;
            Some((parsed, (tv.name.clone(), tv.automated)))
        })
        .collect();

    // ---- Profile rows: system, then user. The default UUID always
    // sorts first; everything else by friendly name.
    let system_profile_rows = build_profile_rows(
        &system_profile_bodies,
        DEFAULT_SYSTEM_PROFILE_UUID,
        &tv_assignments,
        &tv_meta,
        ProfileKindForRefs::System,
    );
    let user_profile_rows = build_profile_rows(
        &user_profile_bodies,
        DEFAULT_USER_PROFILE_UUID,
        &tv_assignments,
        &tv_meta,
        ProfileKindForRefs::User,
    );

    // ComboBox name lists for the TVs pane — friendly names from the
    // body catalogs, sorted with the reserved default first then
    // alphabetical so the picker always shows `default` on top.
    let system_profiles = profile_name_list(&system_profile_bodies, DEFAULT_SYSTEM_PROFILE_UUID);
    let user_profiles = profile_name_list(&user_profile_bodies, DEFAULT_USER_PROFILE_UUID);

    StateSnapshot {
        tvs: models,
        system_profiles,
        user_profiles,
        system_profile_rows,
        user_profile_rows,
    }
}

/// Which side's `references_of` helper to use when computing
/// `referenced_by` for a row. Localized inside this module — the
/// public Slint enum lives in `ui/globals/profiles.slint`.
#[derive(Debug, Clone, Copy)]
enum ProfileKindForRefs {
    System,
    User,
}

/// Build the master-list rows for one profile side (system or user).
///
/// Default UUID always lands first; remaining rows sort by friendly
/// name. Each row carries the fanned-out `referenced_by` list +
/// pre-joined `referenced_by_summary` so Slint can render the chips
/// and the "still used by X, Y" delete-confirm dialog without an
/// array-join expression.
fn build_profile_rows<B>(
    bodies: &HashMap<Uuid, B>,
    default_uuid: Uuid,
    tv_assignments: &[TvAssignment],
    tv_meta: &HashMap<Uuid, (String, bool)>,
    kind: ProfileKindForRefs,
) -> Vec<ProfileRowRaw>
where
    B: HasName,
{
    let mut entries: Vec<(Uuid, &B)> = bodies.iter().map(|(u, b)| (*u, b)).collect();
    entries.sort_by(|a, b| {
        let a_default = a.0 == default_uuid;
        let b_default = b.0 == default_uuid;
        match (a_default, b_default) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.1.name_str().cmp(b.1.name_str()),
        }
    });

    let mut rows = Vec::with_capacity(entries.len());
    for (uuid, body) in entries {
        let referenced_uuids = match kind {
            ProfileKindForRefs::System => references_of(uuid, tv_assignments),
            ProfileKindForRefs::User => user_references_of(uuid, tv_assignments),
        };

        let mut referenced_by = Vec::with_capacity(referenced_uuids.len());
        let mut active = false;
        for tv_uuid in &referenced_uuids {
            if let Some((name, automated)) = tv_meta.get(tv_uuid) {
                referenced_by.push(ProfileTvRefRaw {
                    uuid: SharedString::from(tv_uuid.to_string()),
                    name: SharedString::from(name.as_str()),
                    automated: *automated,
                });
                if *automated {
                    active = true;
                }
            }
        }
        // Alphabetical by friendly name so the Used-by row + the
        // delete-confirm reference-list read consistently across
        // pane visits — the `references_of` helper preserves the TV
        // registration order, which is arbitrary from the user's
        // POV.
        referenced_by.sort_by(|a, b| a.name.cmp(&b.name));
        let summary_parts: Vec<String> = referenced_by.iter().map(|r| r.name.to_string()).collect();

        rows.push(ProfileRowRaw {
            uuid: SharedString::from(uuid.to_string()),
            name: SharedString::from(body.name_str()),
            display_name: SharedString::from(display_profile_name(
                uuid,
                body.name_str(),
                default_uuid,
            )),
            is_default: uuid == default_uuid,
            referenced_by,
            referenced_by_summary: SharedString::from(summary_parts.join(", ")),
            active,
        });
    }
    rows
}

/// Render a profile's name for read-only display surfaces. For the
/// reserved-default UUID, returns the localized "default" word (via
/// gettext on the bare name, which is always the English literal
/// `"default"` for that UUID by daemon contract) followed by a
/// non-breaking space and the default-indicator glyph
/// (U+2713 CHECK MARK). For every other UUID the bare name passes
/// through unchanged — user-typed names aren't gettext candidates.
///
/// The glyph here is **identity-decoupled presentation**: wire
/// storage stays the bare name; only display strings carry the
/// indicator. Baking the glyph into `body.name` on disk would
/// break the daemon's reserved-default predicate (`body.name ==
/// "default"`), surprise CLI / sysadmin consumers reading the
/// on-disk TOML, and tangle the i18n catalog (gettext keys aren't
/// emoji-aware). Synthesis at this layer keeps the wire clean.
pub(crate) fn display_profile_name(uuid: Uuid, name: &str, default_uuid: Uuid) -> String {
    if uuid == default_uuid {
        format!("{}\u{00A0}\u{2713}", gettext(name))
    } else {
        name.to_owned()
    }
}

/// Resolve a profile UUID string to the displayed name shown by the
/// TVs-pane ComboBox's `current-value`. Routes through
/// [`display_profile_name`] so the picker's value matches one of
/// the items in its model — std `ComboBox.current-value` is a
/// string-match against the model, so any drift between this and
/// [`profile_name_list`] would render the field as a blank cell.
/// Unknown / malformed UUIDs fall through to the raw input so a
/// stale-state drift stays visible rather than collapsing.
fn resolve_profile_name<B>(uuid_str: &str, bodies: &HashMap<Uuid, B>, default_uuid: Uuid) -> String
where
    B: HasName,
{
    Uuid::parse_str(uuid_str)
        .ok()
        .and_then(|u| {
            bodies
                .get(&u)
                .map(|b| display_profile_name(u, b.name_str(), default_uuid))
        })
        .unwrap_or_else(|| uuid_str.to_owned())
}

/// Slint-side ComboBox name list. The reserved default profile's
/// label always sorts first so the picker reads consistently.
/// Items are display names — the reserved-default entry carries
/// the indicator glyph appended via [`display_profile_name`].
fn profile_name_list<B>(bodies: &HashMap<Uuid, B>, default_uuid: Uuid) -> Vec<SharedString>
where
    B: HasName,
{
    let mut entries: Vec<(Uuid, String)> = bodies
        .iter()
        .map(|(u, b)| (*u, display_profile_name(*u, b.name_str(), default_uuid)))
        .collect();
    entries.sort_by(|a, b| {
        let a_default = a.0 == default_uuid;
        let b_default = b.0 == default_uuid;
        match (a_default, b_default) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.1.cmp(&b.1),
        }
    });
    entries
        .into_iter()
        .map(|(_, name)| SharedString::from(name))
        .collect()
}

/// Tiny trait so [`build_profile_rows`] and [`profile_name_list`]
/// can read the `name` field off either body type without macro
/// gymnastics — both shapes carry an always-present `String` label
/// (`SystemProfileBody::name`, `UserProfileBody::name`).
trait HasName {
    fn name_str(&self) -> &str;
}

impl HasName for SystemProfileBody {
    fn name_str(&self) -> &str {
        &self.name
    }
}

impl HasName for UserProfileBody {
    fn name_str(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::{DesktopMapping, HardwareId, SessionAction};

    /// Build a `TvInfo` with sensible defaults so each test only
    /// names the fields it cares about. `TvInfo` doesn't derive
    /// `Default` upstream.
    fn sample_tv(uuid: &str, name: &str) -> TvInfo {
        TvInfo {
            uuid: uuid.into(),
            name: name.into(),
            vendor: "lg".into(),
            capabilities: Capabilities::STOCK,
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
            system_profile_bodies: HashMap::new(),
            user_profile_bodies: HashMap::new(),
            user_profile_by_uuid: HashMap::new(),
        }
    }

    fn sys_body(name: &str) -> SystemProfileBody {
        SystemProfileBody {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn user_body(name: &str) -> UserProfileBody {
        UserProfileBody {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_inputs_produce_empty_snapshot() {
        let snap = convert_snapshot(empty_inputs());
        assert!(snap.tvs.is_empty());
        assert!(snap.system_profiles.is_empty());
        assert!(snap.user_profiles.is_empty());
        assert!(snap.system_profile_rows.is_empty());
        assert!(snap.user_profile_rows.is_empty());
    }

    #[test]
    fn single_tv_round_trips_through_conversion() {
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "Living Room");
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
        tv.system_profile = DEFAULT_SYSTEM_PROFILE_UUID.to_string();
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
        assert_eq!(row.uuid.as_str(), "00000000-0000-0000-0000-000000000001");
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
        assert_eq!(
            row.system_profile.as_str(),
            DEFAULT_SYSTEM_PROFILE_UUID.to_string()
        );
        assert_eq!(row.hw_manufacturer.as_str(), "GSM");
        assert_eq!(row.hw_model.as_str(), "OLED65G4SUB");
        assert_eq!(row.hw_serial.as_str(), "XYZ123");
    }

    #[test]
    fn capabilities_map_to_per_control_has_flags() {
        // A sub-STOCK backend (CEC: power + input only) must surface
        // the per-control flags the TVs-pane uses to hide the PICTURE /
        // AUDIO controls it can't drive.
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "CEC TV");
        tv.capabilities = Capabilities::POWER | Capabilities::INPUT;
        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        let row = &snap.tvs[0];
        assert!(row.has_input);
        assert!(!row.has_volume);
        assert!(!row.has_mute);
        assert!(!row.has_backlight);
        assert!(!row.has_screen);

        // A full STOCK backend lights every control up.
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000002", "LG TV");
        tv.capabilities = Capabilities::STOCK;
        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        let row = &snap.tvs[0];
        assert!(row.has_input);
        assert!(row.has_volume);
        assert!(row.has_mute);
        assert!(row.has_backlight);
        assert!(row.has_screen);
    }

    #[test]
    fn missing_hardware_id_fields_become_empty_strings() {
        let tv = sample_tv("00000000-0000-0000-0000-000000000001", "TV");
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
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "TV");
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
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "TV");
        tv.vendor_info = vec![("scheme".into(), "wss".into())];

        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        assert_eq!(snap.tvs[0].primary_descriptor.as_str(), "wss");
    }

    #[test]
    fn primary_descriptor_empty_when_both_lists_empty() {
        let tv = sample_tv("00000000-0000-0000-0000-000000000001", "TV");
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
                sample_tv("00000000-0000-0000-0000-00000000000c", "Bedroom"),
                sample_tv("00000000-0000-0000-0000-00000000000a", "Office"),
                sample_tv("00000000-0000-0000-0000-00000000000b", "Attic"),
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
            tvs: vec![
                sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A"),
                sample_tv("00000000-0000-0000-0000-00000000000b", "TV-B"),
            ],
            user_profile_by_uuid: HashMap::from([(
                "00000000-0000-0000-0000-00000000000a".into(),
                "movies".into(),
            )]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let by_uuid: HashMap<_, _> = snap
            .tvs
            .iter()
            .map(|t| (t.uuid.to_string(), t.user_profile.to_string()))
            .collect();
        assert_eq!(by_uuid["00000000-0000-0000-0000-00000000000a"], "movies");
        assert_eq!(by_uuid["00000000-0000-0000-0000-00000000000b"], "");
    }

    #[test]
    fn tv_system_profile_field_resolves_uuid_to_name() {
        // `Tv1.SystemProfile` carries the UUID; the TVs-pane combobox
        // renders `current-value` verbatim against the names model, so
        // `convert_snapshot` must do the UUID -> name translation.
        // The mirror `system_profile_uuid` field carries the immutable
        // UUID for the navigation pencil.
        let other_uuid = Uuid::new_v4();
        let mut tv_a = sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A");
        tv_a.system_profile = DEFAULT_SYSTEM_PROFILE_UUID.to_string();
        let mut tv_b = sample_tv("00000000-0000-0000-0000-00000000000b", "TV-B");
        tv_b.system_profile = other_uuid.to_string();
        let inputs = SnapshotInputs {
            tvs: vec![tv_a, tv_b],
            system_profile_bodies: HashMap::from([
                (DEFAULT_SYSTEM_PROFILE_UUID, sys_body("default")),
                (other_uuid, sys_body("evening")),
            ]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let by_uuid: HashMap<_, _> = snap
            .tvs
            .iter()
            .map(|t| {
                (
                    t.uuid.to_string(),
                    (
                        t.system_profile.to_string(),
                        t.system_profile_uuid.to_string(),
                    ),
                )
            })
            .collect();
        // The reserved-default row carries the indicator glyph via
        // display_profile_name; route the expectation through the
        // same helper to stay glyph-agnostic.
        let default_display = display_profile_name(
            DEFAULT_SYSTEM_PROFILE_UUID,
            "default",
            DEFAULT_SYSTEM_PROFILE_UUID,
        );
        assert_eq!(
            by_uuid["00000000-0000-0000-0000-00000000000a"].0,
            default_display
        );
        assert_eq!(
            by_uuid["00000000-0000-0000-0000-00000000000a"].1,
            DEFAULT_SYSTEM_PROFILE_UUID.hyphenated().to_string()
        );
        assert_eq!(by_uuid["00000000-0000-0000-0000-00000000000b"].0, "evening");
        assert_eq!(
            by_uuid["00000000-0000-0000-0000-00000000000b"].1,
            other_uuid.hyphenated().to_string()
        );
    }

    #[test]
    fn tv_with_empty_profile_assignment_collapses_to_default_uuid() {
        // A TV with no `Tv1.SystemProfile` set surfaces an empty
        // string on the wire. The navigation pencil needs a real UUID
        // to route on, so `system_profile_uuid` collapses to the
        // reserved-default UUID rather than `""`.
        let tv = sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A");
        // Leave tv.system_profile as default (empty String from sample_tv).
        let inputs = SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        assert_eq!(
            snap.tvs[0].system_profile_uuid.as_str(),
            DEFAULT_SYSTEM_PROFILE_UUID.hyphenated().to_string()
        );
        assert_eq!(
            snap.tvs[0].user_profile_uuid.as_str(),
            DEFAULT_USER_PROFILE_UUID.hyphenated().to_string()
        );
    }

    #[test]
    fn tv_system_profile_field_falls_through_on_unknown_uuid() {
        // A TV assigned to a profile no longer in the catalog (stale-
        // state drift) keeps showing the raw UUID so the broken state
        // stays visible rather than blanking the cell.
        let stale = "11111111-1111-1111-1111-111111111111";
        let mut tv = sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A");
        tv.system_profile = stale.into();
        let inputs = SnapshotInputs {
            tvs: vec![tv],
            system_profile_bodies: HashMap::from([(
                DEFAULT_SYSTEM_PROFILE_UUID,
                sys_body("default"),
            )]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        assert_eq!(snap.tvs[0].system_profile.as_str(), stale);
    }

    #[test]
    fn tv_user_profile_field_resolves_uuid_to_name() {
        // Same shape as the system test, but the user side feeds
        // `convert_snapshot` via `user_profile_by_uuid` (the wire
        // assignment from `Session1.GetTvUserProfile`).
        let movies_uuid = Uuid::new_v4();
        let inputs = SnapshotInputs {
            tvs: vec![sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A")],
            user_profile_by_uuid: HashMap::from([(
                "00000000-0000-0000-0000-00000000000a".into(),
                movies_uuid.to_string(),
            )]),
            user_profile_bodies: HashMap::from([(movies_uuid, user_body("movies"))]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        assert_eq!(snap.tvs[0].user_profile.as_str(), "movies");
    }

    #[test]
    fn profile_lists_drop_descriptions_and_keep_names() {
        let inputs = SnapshotInputs {
            system_profile_bodies: HashMap::from([
                (DEFAULT_SYSTEM_PROFILE_UUID, sys_body("default")),
                (Uuid::new_v4(), sys_body("evening")),
            ]),
            user_profile_bodies: HashMap::from([(Uuid::new_v4(), user_body("movies"))]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        // Default sorts first (carries the indicator glyph via
        // display_profile_name; the test routes through the same
        // helper to stay glyph-agnostic); "evening" follows
        // alphabetically with the bare name.
        let default_display = display_profile_name(
            DEFAULT_SYSTEM_PROFILE_UUID,
            "default",
            DEFAULT_SYSTEM_PROFILE_UUID,
        );
        assert_eq!(
            snap.system_profiles,
            vec![
                SharedString::from(default_display),
                SharedString::from("evening"),
            ]
        );
        assert_eq!(snap.user_profiles, vec![SharedString::from("movies")]);
    }

    #[test]
    fn profile_rows_sort_default_first_then_alpha() {
        let other = Uuid::new_v4();
        let alpha = Uuid::new_v4();
        let inputs = SnapshotInputs {
            system_profile_bodies: HashMap::from([
                (other, sys_body("zen")),
                (DEFAULT_SYSTEM_PROFILE_UUID, sys_body("default")),
                (alpha, sys_body("alpha")),
            ]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        let order: Vec<_> = snap
            .system_profile_rows
            .iter()
            .map(|r| r.name.to_string())
            .collect();
        assert_eq!(order, vec!["default", "alpha", "zen"]);
        assert!(snap.system_profile_rows[0].is_default);
        assert!(!snap.system_profile_rows[1].is_default);
    }

    #[test]
    fn default_profile_fans_in_unassigned_tvs() {
        // The fan-out semantic from `references_of`: a TV with an empty
        // (unparseable / unset) `system_profile` field implicitly
        // references the reserved default UUID. The convert pipeline
        // must surface that in the default row's `referenced_by`.
        let tv_uuid = Uuid::new_v4();
        let tv = {
            let mut t = sample_tv(&tv_uuid.to_string(), "Unassigned-TV");
            t.system_profile = String::new(); // unparseable → None on the assignment
            t.automated = true;
            t
        };
        let inputs = SnapshotInputs {
            tvs: vec![tv],
            system_profile_bodies: HashMap::from([(
                DEFAULT_SYSTEM_PROFILE_UUID,
                sys_body("default"),
            )]),
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        assert_eq!(snap.system_profile_rows.len(), 1);
        let row = &snap.system_profile_rows[0];
        assert!(row.is_default);
        // The unassigned TV must show up under the default profile's
        // `referenced_by` — that's the fan-out branch in references_of.
        assert_eq!(
            row.referenced_by.len(),
            1,
            "unassigned TV should fan out to default"
        );
        assert_eq!(row.referenced_by[0].name.as_str(), "Unassigned-TV");
        assert_eq!(row.referenced_by_summary.as_str(), "Unassigned-TV");
        assert!(row.active, "automated TV should mark the row active");
    }

    #[test]
    fn user_default_profile_fans_in_unassigned_tvs() {
        // Mirror of the system-side fan-out test for the user side.
        let tv_uuid = Uuid::new_v4();
        let tv = sample_tv(&tv_uuid.to_string(), "TV-X");
        let inputs = SnapshotInputs {
            tvs: vec![tv],
            user_profile_bodies: HashMap::from([(DEFAULT_USER_PROFILE_UUID, user_body("default"))]),
            // user_profile_by_uuid intentionally empty → user_profile = None
            ..empty_inputs()
        };
        let snap = convert_snapshot(inputs);
        assert_eq!(snap.user_profile_rows.len(), 1);
        let row = &snap.user_profile_rows[0];
        assert!(row.is_default);
        assert_eq!(row.referenced_by.len(), 1);
        assert_eq!(row.referenced_by[0].name.as_str(), "TV-X");
    }

    #[test]
    fn body_field_round_trip_via_fetch_helpers_compiles() {
        // Pure-static check that the body helpers' signatures keep
        // matching the wire wrappers; constructed bodies are used by
        // the snapshot's row builder.
        let _b = SystemProfileBody {
            name: "guest".into(),
            on_startup: Some(SessionAction::PowerOn),
            ..Default::default()
        };
        let _u = UserProfileBody {
            name: "movies".into(),
            idle_timeout_seconds: Some(300),
            ..Default::default()
        };
    }
}
