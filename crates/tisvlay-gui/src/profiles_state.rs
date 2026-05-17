//! Profile editor state + inheritance computation for the Profiles
//! pane.
//!
//! Holds the three layers that combine to produce the
//! `(inherit: <X>)` placeholder + `current-value` on each
//! [`ProfileField`] row:
//!
//! 1. **Baseline** — hardcoded
//!    [`ResolvedSystemProfile::default()`] fetched once via
//!    `Manager1.GetResolvedProfileDefaults`. Constant for the
//!    lifetime of a daemon binary; refetched on `DaemonAppeared`.
//! 2. **Default body** — the user's overrides for the "default"
//!    profile, loaded with `Manager1.GetSystemProfile("default")`.
//!    Second inheritance layer for every non-default profile.
//! 3. **Selected body** — the body of the profile being edited.
//!    Loaded with `Manager1.GetSystemProfile(name)` on selection.
//!
//! Pure functions ([`compute_system_field_rows`],
//! [`apply_system_field_edit`]) are unit-tested without the GUI; the
//! async refresh + Slint push live in `state_watch.rs` / `lib.rs`.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use slint::SharedString;
use tisvlay_core::{
    AutomateWhen, DaemonClient, ResolvedSystemProfile, SessionAction, SystemProfileConfig,
};
use tokio::sync::RwLock;
use tracing::warn;

/// Raw shape that the Slint `ProfileField` model wraps; identical
/// fields, just `SharedString` carried out of the event loop on the
/// tokio side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileFieldRowRaw {
    pub field_name: SharedString,
    pub label: SharedString,
    pub options: Vec<SharedString>,
    pub current_value: SharedString,
}

/// Editor-side cache. One instance per GUI, shared between the
/// `Profiles.select` and `Profiles.field-edited` handlers (read +
/// write under tokio::RwLock) and the `apply_snapshot` push (read
/// only). Empty by default — `refresh_system_editor` fills it.
#[derive(Debug, Default, Clone)]
pub struct ProfilesEditorState {
    pub system_baseline: ResolvedSystemProfile,
    /// Body of the "default" system profile (the second inheritance
    /// layer between the hardcoded baseline and any named profile).
    pub system_default_body: SystemProfileConfig,
    /// Currently-selected system profile (name + body). `None` when
    /// no system profile is selected or the daemon fetch is in
    /// flight.
    pub system_selected: Option<(String, SystemProfileConfig)>,
}

pub type SharedEditorState = Arc<RwLock<ProfilesEditorState>>;

/// Refetch every layer the editor needs and update the shared cache.
///
/// Always refreshes the baseline + "default" body so a stale cache
/// doesn't cause the `(inherit: X)` placeholders to lie after the
/// underlying `default` profile is edited (by this client or
/// another). The named profile is also refetched so an external
/// edit lands without a full reload.
///
/// `selected_name` is the profile to load into `system_selected`.
/// Passing `None` clears the selection.
pub async fn refresh_system_editor(
    daemon: &DaemonClient,
    state: &SharedEditorState,
    selected_name: Option<&str>,
) -> Result<()> {
    let baseline = daemon
        .get_resolved_system_profile_defaults()
        .await
        .context("Failed to fetch resolved system-profile defaults")?;
    let default_body = daemon
        .get_system_profile("default")
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "Failed to fetch 'default' system profile body — using empty");
            SystemProfileConfig::default()
        });
    let selected = match selected_name {
        Some(name) => {
            let body = daemon
                .get_system_profile(name)
                .await
                .with_context(|| format!("Failed to fetch system profile '{name}'"))?;
            Some((name.to_string(), body))
        }
        None => None,
    };

    let mut s = state.write().await;
    s.system_baseline = baseline;
    s.system_default_body = default_body;
    s.system_selected = selected;
    Ok(())
}

// =============================================================================
// Field row computation
// =============================================================================

/// Build the `ProfileField` rows for the currently-selected system
/// profile. Returns an empty Vec when no system profile is selected.
pub fn compute_system_field_rows(state: &ProfilesEditorState) -> Vec<ProfileFieldRowRaw> {
    let Some((name, body)) = state.system_selected.as_ref() else {
        return Vec::new();
    };
    let editing_default = name == "default";
    let baseline = &state.system_baseline;
    let default = &state.system_default_body;

    vec![
        session_action_row(
            "on_startup",
            "On startup",
            body.on_startup,
            fallthrough_action(editing_default, default.on_startup, baseline.on_startup),
        ),
        session_action_row(
            "on_shutdown",
            "On shutdown",
            body.on_shutdown,
            fallthrough_action(editing_default, default.on_shutdown, baseline.on_shutdown),
        ),
        session_action_row(
            "on_reboot",
            "On reboot",
            body.on_reboot,
            fallthrough_action(editing_default, default.on_reboot, baseline.on_reboot),
        ),
        session_action_row(
            "on_no_session",
            "On no session",
            body.on_no_session,
            fallthrough_action(
                editing_default,
                default.on_no_session,
                baseline.on_no_session,
            ),
        ),
        session_action_row(
            "on_sleep",
            "On sleep",
            body.on_sleep,
            fallthrough_action(editing_default, default.on_sleep, baseline.on_sleep),
        ),
        session_action_row(
            "on_wake",
            "On wake",
            body.on_wake,
            fallthrough_action(editing_default, default.on_wake, baseline.on_wake),
        ),
        automate_when_row(
            "automate_when",
            "Automate when",
            body.automate_when,
            fallthrough_when(
                editing_default,
                default.automate_when,
                baseline.automate_when,
            ),
        ),
    ]
}

/// Apply one field edit to a system-profile body. `value` is the
/// wire shape from the Slint side: empty string clears the field
/// back to `None`; otherwise it's the enum's serde representation
/// (`"power_off"` etc.).
pub fn apply_system_field_edit(
    body: &mut SystemProfileConfig,
    field_name: &str,
    value: &str,
) -> Result<()> {
    match field_name {
        "on_startup" => body.on_startup = parse_action(value)?,
        "on_shutdown" => body.on_shutdown = parse_action(value)?,
        "on_reboot" => body.on_reboot = parse_action(value)?,
        "on_no_session" => body.on_no_session = parse_action(value)?,
        "on_sleep" => body.on_sleep = parse_action(value)?,
        "on_wake" => body.on_wake = parse_action(value)?,
        "automate_when" => body.automate_when = parse_when(value)?,
        _ => bail!("unknown system-profile field: {field_name}"),
    }
    Ok(())
}

// =============================================================================
// Helpers
// =============================================================================

/// Inheritance fallthrough for a `SessionAction` field.
///
/// When editing "default": the only fallthrough is the hardcoded
/// baseline. When editing any other profile: fall through to
/// "default"'s value if set, else to the baseline.
fn fallthrough_action(
    editing_default: bool,
    default_value: Option<SessionAction>,
    baseline: SessionAction,
) -> SessionAction {
    if editing_default {
        baseline
    } else {
        default_value.unwrap_or(baseline)
    }
}

fn fallthrough_when(
    editing_default: bool,
    default_value: Option<AutomateWhen>,
    baseline: AutomateWhen,
) -> AutomateWhen {
    if editing_default {
        baseline
    } else {
        default_value.unwrap_or(baseline)
    }
}

fn session_action_row(
    field_name: &str,
    label: &str,
    current: Option<SessionAction>,
    fallthrough: SessionAction,
) -> ProfileFieldRowRaw {
    let mut options = Vec::with_capacity(6);
    options.push(SharedString::from(format!(
        "(inherit: {})",
        action_str(fallthrough)
    )));
    for v in [
        SessionAction::PowerOn,
        SessionAction::PowerOff,
        SessionAction::ScreenOn,
        SessionAction::ScreenOff,
        SessionAction::None,
    ] {
        options.push(SharedString::from(action_str(v)));
    }
    ProfileFieldRowRaw {
        field_name: SharedString::from(field_name),
        label: SharedString::from(label),
        options,
        current_value: current
            .map(|a| SharedString::from(action_str(a)))
            .unwrap_or_default(),
    }
}

fn automate_when_row(
    field_name: &str,
    label: &str,
    current: Option<AutomateWhen>,
    fallthrough: AutomateWhen,
) -> ProfileFieldRowRaw {
    let mut options = Vec::with_capacity(4);
    options.push(SharedString::from(format!(
        "(inherit: {})",
        when_str(fallthrough)
    )));
    for v in [
        AutomateWhen::Always,
        AutomateWhen::OnPrimary,
        AutomateWhen::OffPrimary,
    ] {
        options.push(SharedString::from(when_str(v)));
    }
    ProfileFieldRowRaw {
        field_name: SharedString::from(field_name),
        label: SharedString::from(label),
        options,
        current_value: current
            .map(|w| SharedString::from(when_str(w)))
            .unwrap_or_default(),
    }
}

fn action_str(a: SessionAction) -> &'static str {
    match a {
        SessionAction::None => "none",
        SessionAction::ScreenOff => "screen_off",
        SessionAction::ScreenOn => "screen_on",
        SessionAction::PowerOff => "power_off",
        SessionAction::PowerOn => "power_on",
    }
}

fn action_from_str(s: &str) -> Option<SessionAction> {
    match s {
        "none" => Some(SessionAction::None),
        "screen_off" => Some(SessionAction::ScreenOff),
        "screen_on" => Some(SessionAction::ScreenOn),
        "power_off" => Some(SessionAction::PowerOff),
        "power_on" => Some(SessionAction::PowerOn),
        _ => None,
    }
}

fn when_str(w: AutomateWhen) -> &'static str {
    match w {
        AutomateWhen::Always => "always",
        AutomateWhen::OnPrimary => "on_primary",
        AutomateWhen::OffPrimary => "off_primary",
    }
}

fn when_from_str(s: &str) -> Option<AutomateWhen> {
    match s {
        "always" => Some(AutomateWhen::Always),
        "on_primary" => Some(AutomateWhen::OnPrimary),
        "off_primary" => Some(AutomateWhen::OffPrimary),
        _ => None,
    }
}

fn parse_action(value: &str) -> Result<Option<SessionAction>> {
    if value.is_empty() {
        Ok(None)
    } else {
        action_from_str(value)
            .map(Some)
            .with_context(|| format!("invalid SessionAction: {value}"))
    }
}

fn parse_when(value: &str) -> Result<Option<AutomateWhen>> {
    if value.is_empty() {
        Ok(None)
    } else {
        when_from_str(value)
            .map(Some)
            .with_context(|| format!("invalid AutomateWhen: {value}"))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_selected(name: &str, body: SystemProfileConfig) -> ProfilesEditorState {
        ProfilesEditorState {
            system_baseline: ResolvedSystemProfile::default(),
            system_default_body: SystemProfileConfig::default(),
            system_selected: Some((name.to_string(), body)),
        }
    }

    #[test]
    fn no_selection_produces_empty_field_list() {
        let state = ProfilesEditorState::default();
        assert!(compute_system_field_rows(&state).is_empty());
    }

    #[test]
    fn default_profile_fallthrough_uses_hardcoded_baseline() {
        // Editing "default" with no fields set — fallthrough on every
        // row should be the baseline's value, not None / blank.
        let state = state_with_selected("default", SystemProfileConfig::default());
        let rows = compute_system_field_rows(&state);

        let on_startup = rows.iter().find(|r| r.field_name == "on_startup").unwrap();
        assert_eq!(
            on_startup.options[0].as_str(),
            "(inherit: power_on)",
            "default's on_startup falls back to baseline (PowerOn)"
        );
        assert_eq!(
            on_startup.current_value.as_str(),
            "",
            "no override → empty current-value (combobox shows inherit placeholder)"
        );
    }

    #[test]
    fn non_default_profile_fallthrough_routes_through_default_body() {
        // "guest" is being edited; "default" overrides on_shutdown to
        // ScreenOff. guest's on_shutdown row should show
        // `(inherit: screen_off)`, not the baseline's `power_off`.
        let mut state = state_with_selected("guest", SystemProfileConfig::default());
        state.system_default_body.on_shutdown = Some(SessionAction::ScreenOff);
        let rows = compute_system_field_rows(&state);

        let on_shutdown = rows.iter().find(|r| r.field_name == "on_shutdown").unwrap();
        assert_eq!(on_shutdown.options[0].as_str(), "(inherit: screen_off)");

        // on_startup wasn't touched on "default" → falls all the way
        // through to the baseline.
        let on_startup = rows.iter().find(|r| r.field_name == "on_startup").unwrap();
        assert_eq!(on_startup.options[0].as_str(), "(inherit: power_on)");
    }

    #[test]
    fn locally_set_value_surfaces_in_current_value() {
        let body = SystemProfileConfig {
            on_sleep: Some(SessionAction::ScreenOff),
            ..Default::default()
        };
        let state = state_with_selected("guest", body);
        let rows = compute_system_field_rows(&state);

        let on_sleep = rows.iter().find(|r| r.field_name == "on_sleep").unwrap();
        assert_eq!(on_sleep.current_value.as_str(), "screen_off");
    }

    #[test]
    fn automate_when_row_uses_when_options_not_action_options() {
        let state = state_with_selected("default", SystemProfileConfig::default());
        let rows = compute_system_field_rows(&state);

        let row = rows
            .iter()
            .find(|r| r.field_name == "automate_when")
            .unwrap();
        // options[0] is the inherit placeholder; options[1..] are the
        // concrete AutomateWhen variants, not SessionAction.
        let concrete: Vec<&str> = row.options[1..].iter().map(|s| s.as_str()).collect();
        assert_eq!(concrete, vec!["always", "on_primary", "off_primary"]);
    }

    #[test]
    fn apply_edit_sets_session_action_field_to_some() {
        let mut body = SystemProfileConfig::default();
        apply_system_field_edit(&mut body, "on_startup", "power_off").unwrap();
        assert_eq!(body.on_startup, Some(SessionAction::PowerOff));
    }

    #[test]
    fn apply_edit_with_empty_value_clears_field_to_none() {
        let mut body = SystemProfileConfig {
            on_startup: Some(SessionAction::PowerOn),
            ..Default::default()
        };
        apply_system_field_edit(&mut body, "on_startup", "").unwrap();
        assert_eq!(body.on_startup, None);
    }

    #[test]
    fn apply_edit_routes_automate_when_to_its_own_field() {
        let mut body = SystemProfileConfig::default();
        apply_system_field_edit(&mut body, "automate_when", "on_primary").unwrap();
        assert_eq!(body.automate_when, Some(AutomateWhen::OnPrimary));
    }

    #[test]
    fn apply_edit_rejects_unknown_field() {
        let mut body = SystemProfileConfig::default();
        let err =
            apply_system_field_edit(&mut body, "totally_made_up_field", "power_off").unwrap_err();
        assert!(format!("{err:#}").contains("unknown system-profile field"));
    }

    #[test]
    fn apply_edit_rejects_unknown_enum_variant() {
        let mut body = SystemProfileConfig::default();
        let err = apply_system_field_edit(&mut body, "on_startup", "explode").unwrap_err();
        assert!(format!("{err:#}").contains("invalid SessionAction"));
    }

    #[test]
    fn field_count_matches_system_profile_field_count() {
        // 6 SessionAction-typed + 1 AutomateWhen-typed = 7 rows.
        // Bumps in `SystemProfileConfig` should trip this test before
        // the Profiles pane silently drops a new field.
        let state = state_with_selected("default", SystemProfileConfig::default());
        assert_eq!(compute_system_field_rows(&state).len(), 7);
    }
}
