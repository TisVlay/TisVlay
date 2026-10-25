//! Fetches TV state from the daemon and maps it into the Slint
//! `TvViewModel` struct generated from `ui/main.slint`.
//!
//! The fetched data lives in plain `Vec`-based structs (`TvViewRaw`,
//! `ProfileRowRaw`) so it crosses the tokio→Slint thread boundary as
//! `Send`. The Slint event-loop closure then wraps the inner per-row
//! lists into `ModelRc` (which holds `Rc`, `!Send`).
//!
//! [`fetch_state`] reads the clients' mirrors and
//! hands a [`SnapshotInputs`] off to the pure [`convert_snapshot`]
//! function. Tests construct `SnapshotInputs` directly and assert on
//! the resulting `StateSnapshot` without touching the wire.

use std::collections::HashMap;

use gettextrs::gettext;
use slint::SharedString;
use tisvlay::{DaemonClient, SessionClient, SystemProfileBody, TvInfo, UserProfileBody};
use tisvlay_core::{
    Capabilities, DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID, PowerState,
};
use tisvlay_runtime::client::{TvAssignment, references_of, user_references_of};
use tracing::debug;
use uuid::Uuid;

/// Per-TV view-model row in a thread-safe shape (no `Rc` inside).
/// Converted to the Slint-generated `TvViewModel` inside the event
/// loop, where `ModelRc::new` is allowed.
pub(crate) struct TvViewRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub vendor: SharedString,
    /// Canonical vendor for branding the tile — mirrors `TvInfo::brand`.
    pub brand: SharedString,
    /// Backend's primary diagnostic descriptor — first vendor-config
    /// row (`get-config` rpc), falling back to the read-only
    /// diagnostic list. LG: IP address. Future Bluetooth: MAC.
    /// Future USB-CEC: serial-port path. Rendered as the small
    /// monospace hint beside the TV name in the master list.
    pub primary_descriptor: SharedString,
    pub connected: bool,
    /// What a disconnected TV waits for (`TvInfo::waiting_for`): `"network"`,
    /// `"device"`, or empty when nothing is known missing.
    pub waiting_for: SharedString,
    pub power_state: SharedString,
    /// Connected *and* awake (via `PowerState::is_awake`, the CLI's and
    /// tray's rule too). The power button keys on this rather than
    /// `connected` so a connected-in-standby CEC TV still offers "Power
    /// on", and so does a remote-off LG in `activestandby`.
    pub is_on: bool,
    pub supports_wake: bool,
    /// Per-capability flags from the identity payload's capability
    /// list (mirrors [`TvInfo::capabilities`]), used to hide
    /// controls a backend can't
    /// drive — a CEC TV has no volume, mute, backlight, or screen-blank.
    /// Static per vendor, so controls don't flicker as the TV's
    /// connection cycles.
    pub has_screen: bool,
    pub has_volume: bool,
    pub has_mute: bool,
    pub has_input: bool,
    pub has_backlight: bool,
    pub has_remote_power: bool,
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
    /// User-editable vendor-config rows from the `get-config` rpc
    /// payload (LG: IP, MAC). Rendered in the CONFIG group's view-mode
    /// list; the edit form replaces this with `EditPane.fields`
    /// when the user clicks the pencil. Plain
    /// `Vec<(String, String)>` here so the snapshot crosses the
    /// tokio→Slint thread boundary as `Send`; the conversion to
    /// `[VendorInfoRow]` (a `ModelRc`-backed list) happens inside
    /// the event-loop closure in `main.rs`.
    pub vendor_config: Vec<(String, String)>,
    /// Backend-discovered diagnostic rows (`TvInfo::vendor_info`;
    /// LG: WebSocket scheme). Read-only; rendered in the INFO
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
pub(crate) struct ProfileTvRefRaw {
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
pub(crate) struct ProfileRowRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub display_name: SharedString,
    pub is_default: bool,
    pub referenced_by: Vec<ProfileTvRefRaw>,
    pub referenced_by_summary: SharedString,
    pub active: bool,
}

/// Snapshot of fetched state for one refresh pass.
pub(crate) struct StateSnapshot {
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
pub(crate) struct SnapshotInputs {
    pub tvs: Vec<TvInfo>,
    /// System-profile catalog, keyed by parsed UUID. From the
    /// daemon's `profile/system/<uuid>` keys.
    pub system_profile_bodies: HashMap<Uuid, SystemProfileBody>,
    /// User-profile catalog, keyed by parsed UUID. From the session
    /// service's `user/profile/<uuid>` keys. Empty when
    /// the user socket isn't available.
    pub user_profile_bodies: HashMap<Uuid, UserProfileBody>,
    /// Per-TV user-profile assignment, keyed by TV UUID. A TV without
    /// one (no session, or no assignment) is absent and renders an
    /// empty `TvViewRaw.user_profile`.
    pub user_profile_by_uuid: HashMap<Uuid, Uuid>,
}

/// Read every TV's state plus the session-side user-profile assignments
/// and both profile catalogs, all from the clients' mirrors: no rpcs, so
/// the watch loop renders inline without waiting on a service. A daemon
/// on another release lists no TVs.
pub(crate) async fn fetch_state(
    daemon: &DaemonClient,
    session: Option<&SessionClient>,
) -> StateSnapshot {
    let tvs = daemon.list_tvs().await.unwrap_or_else(|e| {
        debug!(error = %e, "TV list unavailable");
        Vec::new()
    });
    debug!(count = tvs.len(), "Read TVs from the daemon mirror");

    let system_profile_bodies = daemon.list_system_profile_bodies().await;

    let user_profile_bodies = match session {
        Some(s) => s.list_user_profile_bodies().await,
        None => HashMap::new(),
    };

    let mut user_profile_by_uuid = HashMap::new();
    if let Some(s) = session {
        // Assignments come from the session client's mirror.
        for tv in &tvs {
            if let Some(profile) = s.get_tv_user_profile(tv.uuid).await {
                user_profile_by_uuid.insert(tv.uuid, profile);
            }
        }
    }

    convert_snapshot(SnapshotInputs {
        tvs,
        system_profile_bodies,
        user_profile_bodies,
        user_profile_by_uuid,
    })
}

/// Fetch a single system-profile body from the mirror, so callers can
/// refresh one row on a `SystemProfileChanged` event without a full
/// catalog list. `None` when the mirror doesn't hold it (yet).
pub(crate) async fn fetch_system_profile_body(
    daemon: &DaemonClient,
    uuid: Uuid,
) -> Option<SystemProfileBody> {
    daemon.get_system_profile(uuid).await
}

/// User-side mirror of [`fetch_system_profile_body`].
pub(crate) async fn fetch_user_profile_body(
    session: &SessionClient,
    uuid: Uuid,
) -> Option<UserProfileBody> {
    session.get_user_profile(uuid).await
}

/// A power state's wire token (`active`, `standby`, ...), which the Slint
/// side keys its labels on.
fn power_state_token(state: PowerState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|token| token.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Pure renderer: shape `SnapshotInputs` into the `StateSnapshot` the
/// GUI consumes. No IO; testable without an event loop.
pub(crate) fn convert_snapshot(inputs: SnapshotInputs) -> StateSnapshot {
    let SnapshotInputs {
        tvs,
        system_profile_bodies,
        user_profile_bodies,
        user_profile_by_uuid,
    } = inputs;

    let mut models = Vec::with_capacity(tvs.len());
    for tv in &tvs {
        let user_profile = user_profile_by_uuid.get(&tv.uuid).copied();

        // Pre-resolved UUIDs for the PROFILES-row pencil. No assignment
        // collapses to the reserved-default UUID so the Slint click
        // handler can route unconditionally without a `== "" ? default`
        // fallback.
        let system_profile_uuid = tv
            .system_profile
            .unwrap_or(DEFAULT_SYSTEM_PROFILE_UUID)
            .hyphenated()
            .to_string();
        let user_profile_uuid = user_profile
            .unwrap_or(DEFAULT_USER_PROFILE_UUID)
            .hyphenated()
            .to_string();

        let available_input_ids: Vec<SharedString> = tv
            .inputs
            .iter()
            .map(|(id, _)| SharedString::from(id.as_str()))
            .collect();

        // Spatial hint shown alongside the name — backend's primary
        // identifying descriptor (LG: IP; CEC: device node), published on
        // the TV's identity from the connector's `primary_descriptor`.
        // Empty renders as "—"; row order carries no meaning and is never
        // consulted.
        let primary_descriptor = tv.primary_descriptor.clone();

        models.push(TvViewRaw {
            uuid: SharedString::from(tv.uuid.to_string()),
            name: SharedString::from(tv.name.as_str()),
            vendor: SharedString::from(tv.vendor.as_str()),
            brand: SharedString::from(tv.brand.as_str()),
            primary_descriptor: SharedString::from(primary_descriptor),
            connected: tv.connected,
            waiting_for: SharedString::from(tv.waiting_for.map_or("", |w| w.as_str())),
            power_state: SharedString::from(power_state_token(tv.power_state)),
            is_on: tv.connected && tv.power_state.is_awake(),
            supports_wake: tv.supports_wake,
            has_screen: tv.capabilities.contains(Capabilities::SCREEN),
            has_volume: tv.capabilities.contains(Capabilities::VOLUME),
            has_mute: tv.capabilities.contains(Capabilities::MUTE),
            has_input: tv.capabilities.contains(Capabilities::INPUT),
            has_backlight: tv.capabilities.contains(Capabilities::BACKLIGHT),
            has_remote_power: tv.capabilities.contains(Capabilities::REMOTE_POWER),
            is_screen_on: tv.is_screen_on,
            volume: i32::from(tv.volume),
            muted: tv.muted,
            backlight: i32::from(tv.backlight),
            current_input: SharedString::from(tv.current_input.as_str()),
            available_input_ids,
            // ComboBox renders `current-value` verbatim. Resolve the
            // wire UUID to the friendly name so the picker matches its
            // `system-profiles` / `user-profiles` model. Unknown UUIDs
            // fall through to the raw string so a stale-state drift
            // stays visible instead of blanking the cell.
            system_profile: SharedString::from(resolve_profile_name(
                tv.system_profile,
                &system_profile_bodies,
                DEFAULT_SYSTEM_PROFILE_UUID,
            )),
            system_profile_uuid: SharedString::from(system_profile_uuid),
            user_profile: SharedString::from(resolve_profile_name(
                user_profile,
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
        .map(|tv| TvAssignment {
            tv_uuid: tv.uuid,
            system_profile: tv.system_profile,
            user_profile: user_profile_by_uuid.get(&tv.uuid).copied(),
        })
        .collect();

    // Per-TV (uuid → (display-name, automated)) lookup used to
    // resolve `referenced_by` chips and the `active` flag without
    // re-iterating the full TV list per row.
    let tv_meta: HashMap<Uuid, (String, bool)> = tvs
        .iter()
        .map(|tv| (tv.uuid, (tv.name.clone(), tv.automated)))
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

/// Resolve an assigned profile to the displayed name shown by the
/// TVs-pane ComboBox's `current-value`. Routes through
/// [`display_profile_name`] so the picker's value matches one of
/// the items in its model — std `ComboBox.current-value` is a
/// string-match against the model, so any drift between this and
/// [`profile_name_list`] would render the field as a blank cell.
/// No assignment renders empty; an unknown UUID falls through to the
/// raw UUID so a stale-state drift stays visible rather than collapsing.
fn resolve_profile_name<B>(
    profile: Option<Uuid>,
    bodies: &HashMap<Uuid, B>,
    default_uuid: Uuid,
) -> String
where
    B: HasName,
{
    let Some(profile) = profile else {
        return String::new();
    };
    bodies
        .get(&profile)
        .map(|b| display_profile_name(profile, b.name_str(), default_uuid))
        .unwrap_or_else(|| profile.to_string())
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
    use tisvlay_core::{HardwareId, SessionAction};

    /// A `TvInfo` with sensible defaults, so each test only names the
    /// fields it cares about. It's `#[non_exhaustive]`, so the fields are
    /// set on a default.
    fn sample_tv(uuid: &str, name: &str) -> TvInfo {
        let mut tv = TvInfo::default();
        tv.uuid = Uuid::parse_str(uuid).unwrap();
        tv.name = name.into();
        tv.vendor = "lg".into();
        tv.brand = "lg".into();
        tv.capabilities = Capabilities::STOCK;
        tv.power_state = PowerState::Unknown;
        tv
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
    fn connected_but_standby_is_not_on() {
        // A CEC TV stays reachable in standby: connected, but off. The
        // power button keys on `is_on`, so this must be false to offer
        // "Power on" rather than a stuck "Power off".
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000009", "CEC TV");
        tv.connected = true;
        tv.power_state = PowerState::Standby;

        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        let row = &snap.tvs[0];
        assert!(row.connected, "still connected on the bus");
        assert!(!row.is_on, "standby is not on");
    }

    #[test]
    fn connected_active_standby_is_not_on() {
        // An LG TV turned off with its remote reports `activestandby`,
        // NIC still up but the panel dark. The power button must offer
        // "Power on", or the user's first press powers off a dark TV.
        let mut tv = sample_tv("00000000-0000-0000-0000-00000000000a", "LG TV");
        tv.connected = true;
        tv.power_state = PowerState::ActiveStandby;

        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        assert!(!snap.tvs[0].is_on, "active standby is not on");
    }

    #[test]
    fn single_tv_round_trips_through_conversion() {
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "Living Room");
        tv.connected = true;
        tv.power_state = PowerState::Active;
        tv.volume = 42;
        tv.backlight = 80;
        tv.muted = true;
        tv.is_screen_on = true;
        tv.current_input = "HDMI_2".into();
        tv.inputs = vec![
            ("HDMI_1".into(), "HDMI 1".into()),
            ("HDMI_2".into(), "HDMI 2".into()),
        ];
        tv.system_profile = Some(DEFAULT_SYSTEM_PROFILE_UUID);
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
        assert!(row.is_on, "connected + active is on");
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
        tv.capabilities = Capabilities::POWER | Capabilities::INPUT | Capabilities::REMOTE_POWER;
        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        let row = &snap.tvs[0];
        assert!(row.has_input);
        assert!(row.has_remote_power);
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
        assert!(!row.has_remote_power, "not in STOCK");
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
    fn primary_descriptor_comes_from_identity_not_row_order() {
        // The published identity carries the connector's primary
        // descriptor; the row lists never influence it — a config field
        // sorting before the vendor's identity field (CEC's
        // advertise_osd_name vs device) must not leak into the pane.
        // Regression for the TVs pane showing "true" instead of /dev/cec0.
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "TV");
        tv.primary_descriptor = "/dev/cec0".into();
        tv.vendor_config = vec![
            ("Advertise input name".into(), "true".into()),
            ("CEC device".into(), "/dev/cec0".into()),
        ];

        let snap = convert_snapshot(SnapshotInputs {
            tvs: vec![tv],
            ..empty_inputs()
        });
        assert_eq!(snap.tvs[0].primary_descriptor.as_str(), "/dev/cec0");
    }

    #[test]
    fn primary_descriptor_defaults_empty() {
        // No published primary (facts not yet evaluated) → empty; the
        // pane renders it as "—". Row lists are never a substitute.
        let mut tv = sample_tv("00000000-0000-0000-0000-000000000001", "TV");
        tv.vendor_config = vec![("IP".into(), "192.168.1.10".into())];
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
        let movies_uuid = Uuid::new_v4();
        let inputs = SnapshotInputs {
            tvs: vec![
                sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A"),
                sample_tv("00000000-0000-0000-0000-00000000000b", "TV-B"),
            ],
            user_profile_by_uuid: HashMap::from([(
                Uuid::parse_str("00000000-0000-0000-0000-00000000000a").unwrap(),
                movies_uuid,
            )]),
            user_profile_bodies: HashMap::from([(movies_uuid, user_body("movies"))]),
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
        // `TvInfo::system_profile` carries the UUID; the TVs-pane combobox
        // renders `current-value` verbatim against the names model, so
        // `convert_snapshot` must do the UUID -> name translation.
        // The mirror `system_profile_uuid` field carries the immutable
        // UUID for the navigation pencil.
        let other_uuid = Uuid::new_v4();
        let mut tv_a = sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A");
        tv_a.system_profile = Some(DEFAULT_SYSTEM_PROFILE_UUID);
        let mut tv_b = sample_tv("00000000-0000-0000-0000-00000000000b", "TV-B");
        tv_b.system_profile = Some(other_uuid);
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
        // A TV with no system profile assigned surfaces an empty
        // `TvInfo::system_profile`. The navigation pencil needs a real UUID
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
        tv.system_profile = Some(Uuid::parse_str(stale).unwrap());
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
        // assignment from `SessionClient::get_tv_user_profile`).
        let movies_uuid = Uuid::new_v4();
        let inputs = SnapshotInputs {
            tvs: vec![sample_tv("00000000-0000-0000-0000-00000000000a", "TV-A")],
            user_profile_by_uuid: HashMap::from([(
                Uuid::parse_str("00000000-0000-0000-0000-00000000000a").unwrap(),
                movies_uuid,
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
        // The fan-out semantic from `references_of`: a TV with no
        // `system_profile` assignment implicitly
        // references the reserved default UUID. The convert pipeline
        // must surface that in the default row's `referenced_by`.
        let tv_uuid = Uuid::new_v4();
        let tv = {
            let mut t = sample_tv(&tv_uuid.to_string(), "Unassigned-TV");
            t.system_profile = None;
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
            on_startup: Some(SessionAction::PowerOn.into()),
            ..Default::default()
        };
        let _u = UserProfileBody {
            name: "movies".into(),
            unlocked_idle_timeout_seconds: Some(300),
            ..Default::default()
        };
    }

    /// The TVs pane keys its status labels on string comparisons against
    /// the row's `power-state`, `power-cycle-phase` and `waiting-for`: a
    /// stringly-typed contract with the wire tokens written into the row.
    /// A literal no token matches never fires, and its label silently
    /// never shows. Parse the literals out of the Slint source and
    /// require each to round-trip through its enum's token.
    const TVS_PANE: &str = include_str!("../../tisvlay-gui/ui/components/tvs_pane.slint");

    fn tvs_pane_literals(property: &str) -> Vec<&'static str> {
        let marker = format!("{property} == \"");
        let literals: Vec<_> = TVS_PANE
            .match_indices(&marker)
            .map(|(idx, _)| {
                TVS_PANE[idx + marker.len()..]
                    .split('"')
                    .next()
                    .expect("opening quote has a close")
            })
            .collect();
        assert!(
            !literals.is_empty(),
            "no `{property} ==` comparisons in the TVs pane"
        );
        literals
    }

    #[test]
    fn tvs_pane_power_state_literals_are_wire_tokens() {
        for literal in tvs_pane_literals("power-state") {
            assert_eq!(
                power_state_token(PowerState::parse(literal)),
                literal,
                "the TVs pane compares power-state against {literal:?}, which no PowerState serializes to"
            );
        }
    }

    #[test]
    fn tvs_pane_power_cycle_phase_literals_are_wire_tokens() {
        for literal in tvs_pane_literals("power-cycle-phase") {
            let phase: tisvlay::PowerPhase = serde_json::from_value(literal.into())
                .unwrap_or_else(|_| panic!("the TVs pane compares power-cycle-phase against {literal:?}, which isn't a PowerPhase"));
            assert_eq!(
                phase.as_str(),
                literal,
                "PowerPhase's as_str drifted from its serde token"
            );
        }
    }

    #[test]
    fn tvs_pane_waiting_for_literals_are_wire_tokens() {
        for literal in tvs_pane_literals("waiting-for") {
            let wait: tisvlay::LinkWait = serde_json::from_value(literal.into())
                .unwrap_or_else(|_| panic!("the TVs pane compares waiting-for against {literal:?}, which isn't a LinkWait"));
            assert_eq!(
                wait.as_str(),
                literal,
                "LinkWait's as_str drifted from its serde token"
            );
        }
    }
}
