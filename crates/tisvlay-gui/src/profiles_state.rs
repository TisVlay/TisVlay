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
    AutomateWhen, DaemonClient, ResolvedSystemProfile, ResolvedUserProfile, SessionAction,
    SessionClient, SystemProfileConfig, UserProfileConfig,
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
/// only). Empty by default — `refresh_system_editor` /
/// `refresh_user_editor` fill it on selection.
#[derive(Debug, Default, Clone)]
pub struct ProfilesEditorState {
    pub system_baseline: ResolvedSystemProfile,
    /// Body of the "default" system profile (the second inheritance
    /// layer between the hardcoded baseline and any named profile).
    pub system_default_body: SystemProfileConfig,
    pub user_baseline: ResolvedUserProfile,
    /// Body of the "default" user profile — second inheritance layer
    /// for non-default user profiles. The session service hides the
    /// admin-baseline-vs-user-config distinction inside what
    /// `GetUserProfile("default")` returns, so the editor only sees
    /// two effective layers (this + the hardcoded baseline).
    pub user_default_body: UserProfileConfig,
    /// What the user is currently editing. The enum encodes the
    /// "at most one profile selected" invariant at the type level —
    /// system_/user_-specific bodies live in their respective
    /// variants so the field-edit handler can dispatch by match.
    pub selected: Option<SelectedProfile>,
}

/// The currently-edited profile. `body` is the unresolved
/// `Option<T>`-fielded shape from the daemon — `None` fields surface
/// as inheriting in the editor, `Some(v)` fields surface as
/// locally-set.
#[derive(Debug, Clone)]
pub enum SelectedProfile {
    System {
        name: String,
        body: SystemProfileConfig,
    },
    User {
        name: String,
        body: UserProfileConfig,
    },
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
            Some(SelectedProfile::System {
                name: name.to_string(),
                body,
            })
        }
        None => None,
    };

    let mut s = state.write().await;
    s.system_baseline = baseline;
    s.system_default_body = default_body;
    s.selected = selected;
    Ok(())
}

/// Mirror of [`refresh_system_editor`] for user profiles. Hits the
/// `Session1` proxy for the baseline + "default" body + named body.
/// On a failed "default" fetch, falls back to an empty body — the
/// inherit chain then bottoms out at the hardcoded baseline, which
/// is the same behavior as the daemon itself.
pub async fn refresh_user_editor(
    session: &SessionClient,
    state: &SharedEditorState,
    selected_name: Option<&str>,
) -> Result<()> {
    let baseline = session
        .get_resolved_profile_defaults()
        .await
        .context("Failed to fetch resolved user-profile defaults")?;
    let default_body = session
        .get_user_profile("default")
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "Failed to fetch 'default' user profile body — using empty");
            UserProfileConfig::default()
        });
    let selected = match selected_name {
        Some(name) => {
            let body = session
                .get_user_profile(name)
                .await
                .with_context(|| format!("Failed to fetch user profile '{name}'"))?;
            Some(SelectedProfile::User {
                name: name.to_string(),
                body,
            })
        }
        None => None,
    };

    let mut s = state.write().await;
    s.user_baseline = baseline;
    s.user_default_body = default_body;
    s.selected = selected;
    Ok(())
}

// =============================================================================
// Field row computation
// =============================================================================

/// Build the `ProfileField` rows for whatever's currently selected.
/// Empty when no selection. Single entry point so the Slint push
/// helper doesn't need to know which kind it's rendering.
pub fn compute_field_rows(state: &ProfilesEditorState) -> Vec<ProfileFieldRowRaw> {
    match state.selected.as_ref() {
        Some(SelectedProfile::System { name, body }) => {
            compute_system_field_rows(name, body, state)
        }
        Some(SelectedProfile::User { name, body }) => compute_user_field_rows(name, body, state),
        None => Vec::new(),
    }
}

fn compute_system_field_rows(
    name: &str,
    body: &SystemProfileConfig,
    state: &ProfilesEditorState,
) -> Vec<ProfileFieldRowRaw> {
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

/// User-side field rows. Five `SessionAction` fields; the
/// `idle_timeout_seconds` numeric field is deferred — it needs a
/// spinner-style widget instead of a ComboBox of presets. See
/// `docs/ideas/profile-user-idle-timeout-spinner.md`.
fn compute_user_field_rows(
    name: &str,
    body: &UserProfileConfig,
    state: &ProfilesEditorState,
) -> Vec<ProfileFieldRowRaw> {
    let editing_default = name == "default";
    let baseline = &state.user_baseline;
    let default = &state.user_default_body;

    vec![
        session_action_row(
            "on_session_lock",
            "On session lock",
            body.on_session_lock,
            fallthrough_action(
                editing_default,
                default.on_session_lock,
                baseline.on_session_lock,
            ),
        ),
        session_action_row(
            "on_session_unlock",
            "On session unlock",
            body.on_session_unlock,
            fallthrough_action(
                editing_default,
                default.on_session_unlock,
                baseline.on_session_unlock,
            ),
        ),
        session_action_row(
            "on_greeter_wake",
            "On greeter wake",
            body.on_greeter_wake,
            fallthrough_action(
                editing_default,
                default.on_greeter_wake,
                baseline.on_greeter_wake,
            ),
        ),
        session_action_row(
            "on_idle_timeout",
            "On idle timeout",
            body.on_idle_timeout,
            fallthrough_action(
                editing_default,
                default.on_idle_timeout,
                baseline.on_idle_timeout,
            ),
        ),
        session_action_row(
            "on_idle_resume",
            "On idle resume",
            body.on_idle_resume,
            fallthrough_action(
                editing_default,
                default.on_idle_resume,
                baseline.on_idle_resume,
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

/// Mirror of [`apply_system_field_edit`] for user profiles. Five
/// `SessionAction` fields supported today; `idle_timeout_seconds`
/// returns an explicit error so a future stray emit doesn't
/// silently no-op (see the spinner-design idea doc).
pub fn apply_user_field_edit(
    body: &mut UserProfileConfig,
    field_name: &str,
    value: &str,
) -> Result<()> {
    match field_name {
        "on_session_lock" => body.on_session_lock = parse_action(value)?,
        "on_session_unlock" => body.on_session_unlock = parse_action(value)?,
        "on_greeter_wake" => body.on_greeter_wake = parse_action(value)?,
        "on_idle_timeout" => body.on_idle_timeout = parse_action(value)?,
        "on_idle_resume" => body.on_idle_resume = parse_action(value)?,
        "idle_timeout_seconds" => {
            bail!("idle_timeout_seconds editor is not implemented yet — edit the TOML directly")
        }
        _ => bail!("unknown user-profile field: {field_name}"),
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

    fn system_state(name: &str, body: SystemProfileConfig) -> ProfilesEditorState {
        ProfilesEditorState {
            selected: Some(SelectedProfile::System {
                name: name.to_string(),
                body,
            }),
            ..Default::default()
        }
    }

    fn user_state(name: &str, body: UserProfileConfig) -> ProfilesEditorState {
        ProfilesEditorState {
            selected: Some(SelectedProfile::User {
                name: name.to_string(),
                body,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn no_selection_produces_empty_field_list() {
        let state = ProfilesEditorState::default();
        assert!(compute_field_rows(&state).is_empty());
    }

    #[test]
    fn default_system_fallthrough_uses_hardcoded_baseline() {
        // Editing "default" with no fields set — fallthrough on every
        // row should be the baseline's value, not None / blank.
        let state = system_state("default", SystemProfileConfig::default());
        let rows = compute_field_rows(&state);

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
    fn non_default_system_fallthrough_routes_through_default_body() {
        // "guest" is being edited; "default" overrides on_shutdown to
        // ScreenOff. guest's on_shutdown row should show
        // `(inherit: screen_off)`, not the baseline's `power_off`.
        let mut state = system_state("guest", SystemProfileConfig::default());
        state.system_default_body.on_shutdown = Some(SessionAction::ScreenOff);
        let rows = compute_field_rows(&state);

        let on_shutdown = rows.iter().find(|r| r.field_name == "on_shutdown").unwrap();
        assert_eq!(on_shutdown.options[0].as_str(), "(inherit: screen_off)");

        // on_startup wasn't touched on "default" → falls all the way
        // through to the baseline.
        let on_startup = rows.iter().find(|r| r.field_name == "on_startup").unwrap();
        assert_eq!(on_startup.options[0].as_str(), "(inherit: power_on)");
    }

    #[test]
    fn locally_set_system_value_surfaces_in_current_value() {
        let body = SystemProfileConfig {
            on_sleep: Some(SessionAction::ScreenOff),
            ..Default::default()
        };
        let state = system_state("guest", body);
        let rows = compute_field_rows(&state);

        let on_sleep = rows.iter().find(|r| r.field_name == "on_sleep").unwrap();
        assert_eq!(on_sleep.current_value.as_str(), "screen_off");
    }

    #[test]
    fn automate_when_row_uses_when_options_not_action_options() {
        let state = system_state("default", SystemProfileConfig::default());
        let rows = compute_field_rows(&state);

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
    fn apply_system_edit_sets_session_action_field_to_some() {
        let mut body = SystemProfileConfig::default();
        apply_system_field_edit(&mut body, "on_startup", "power_off").unwrap();
        assert_eq!(body.on_startup, Some(SessionAction::PowerOff));
    }

    #[test]
    fn apply_system_edit_with_empty_value_clears_field_to_none() {
        let mut body = SystemProfileConfig {
            on_startup: Some(SessionAction::PowerOn),
            ..Default::default()
        };
        apply_system_field_edit(&mut body, "on_startup", "").unwrap();
        assert_eq!(body.on_startup, None);
    }

    #[test]
    fn apply_system_edit_routes_automate_when_to_its_own_field() {
        let mut body = SystemProfileConfig::default();
        apply_system_field_edit(&mut body, "automate_when", "on_primary").unwrap();
        assert_eq!(body.automate_when, Some(AutomateWhen::OnPrimary));
    }

    #[test]
    fn apply_system_edit_rejects_unknown_field() {
        let mut body = SystemProfileConfig::default();
        let err =
            apply_system_field_edit(&mut body, "totally_made_up_field", "power_off").unwrap_err();
        assert!(format!("{err:#}").contains("unknown system-profile field"));
    }

    #[test]
    fn apply_system_edit_rejects_unknown_enum_variant() {
        let mut body = SystemProfileConfig::default();
        let err = apply_system_field_edit(&mut body, "on_startup", "explode").unwrap_err();
        assert!(format!("{err:#}").contains("invalid SessionAction"));
    }

    #[test]
    fn system_field_count_matches_system_profile_field_count() {
        // 6 SessionAction-typed + 1 AutomateWhen-typed = 7 rows.
        // Bumps in `SystemProfileConfig` should trip this test before
        // the Profiles pane silently drops a new field.
        let state = system_state("default", SystemProfileConfig::default());
        assert_eq!(compute_field_rows(&state).len(), 7);
    }

    // ---- User-profile mirror tests ----

    #[test]
    fn default_user_fallthrough_uses_hardcoded_baseline() {
        // Baseline on_session_lock = ScreenOff; editing "default"
        // with no override → fallthrough shows screen_off.
        let state = user_state("default", UserProfileConfig::default());
        let rows = compute_field_rows(&state);

        let row = rows
            .iter()
            .find(|r| r.field_name == "on_session_lock")
            .unwrap();
        assert_eq!(row.options[0].as_str(), "(inherit: screen_off)");
    }

    #[test]
    fn non_default_user_fallthrough_routes_through_default_body() {
        // Editing "movies"; default user profile overrides
        // on_session_lock to PowerOff. movies's row should show
        // (inherit: power_off), not the baseline's screen_off.
        let mut state = user_state("movies", UserProfileConfig::default());
        state.user_default_body.on_session_lock = Some(SessionAction::PowerOff);
        let rows = compute_field_rows(&state);

        let row = rows
            .iter()
            .find(|r| r.field_name == "on_session_lock")
            .unwrap();
        assert_eq!(row.options[0].as_str(), "(inherit: power_off)");
    }

    #[test]
    fn locally_set_user_value_surfaces_in_current_value() {
        let body = UserProfileConfig {
            on_idle_timeout: Some(SessionAction::PowerOff),
            ..Default::default()
        };
        let state = user_state("movies", body);
        let rows = compute_field_rows(&state);

        let row = rows
            .iter()
            .find(|r| r.field_name == "on_idle_timeout")
            .unwrap();
        assert_eq!(row.current_value.as_str(), "power_off");
    }

    #[test]
    fn apply_user_edit_sets_session_action_field_to_some() {
        let mut body = UserProfileConfig::default();
        apply_user_field_edit(&mut body, "on_idle_resume", "screen_on").unwrap();
        assert_eq!(body.on_idle_resume, Some(SessionAction::ScreenOn));
    }

    #[test]
    fn apply_user_edit_idle_timeout_seconds_returns_explicit_error() {
        // Deferred behavior — the editor doesn't render the row, so
        // a stray emit indicates either a future row appeared or a
        // wire-shape bug. Either way we want a hard error, not a
        // silent no-op.
        let mut body = UserProfileConfig::default();
        let err = apply_user_field_edit(&mut body, "idle_timeout_seconds", "300").unwrap_err();
        assert!(format!("{err:#}").contains("idle_timeout_seconds"));
    }

    #[test]
    fn apply_user_edit_rejects_unknown_field() {
        let mut body = UserProfileConfig::default();
        let err =
            apply_user_field_edit(&mut body, "totally_made_up_field", "power_off").unwrap_err();
        assert!(format!("{err:#}").contains("unknown user-profile field"));
    }

    #[test]
    fn user_field_count_excludes_idle_timeout_seconds() {
        // 5 SessionAction-typed rows; idle_timeout_seconds is
        // deferred to a future commit. Bumps in `UserProfileConfig`
        // need to either grow this count (after adding a row) or
        // update this assertion + the deferral doc.
        let state = user_state("default", UserProfileConfig::default());
        assert_eq!(compute_field_rows(&state).len(), 5);
    }
}
