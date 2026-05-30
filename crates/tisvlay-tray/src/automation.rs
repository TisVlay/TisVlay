//! `automation_fired` notification copy + TV-name / profile-name resolution.
//!
//! `Manager1.AutomationFired` (system-profile rules: `on_startup`,
//! `on_sleep`, …) and `Session1.AutomationFired` (user-profile rules:
//! `on_session_lock`, `on_idle_timeout`, …) carry the same five stable
//! snake_case identifiers: `tv_id`, `event_kind`, `action_kind`,
//! `profile_kind` (`"system"` or `"user"`), and `profile_uuid`. The
//! tray resolves the TV's friendly name + the originating profile's
//! display name client-side, then translates the event + action kinds
//! via `gettext` lookup keys so the surfaced notification reads as
//! "<tv name>: <action>" / "Triggered by <event> (<kind> profile:
//! <profile name>)." in the user's locale.

use gettextrs::gettext;
use tisvlay_core::{DaemonClient, SessionClient};
use tokio::sync::RwLock;
use tracing::warn;

/// Look up the TV's friendly name from the daemon. Falls back to the
/// UUID when the lookup fails or the TV isn't in the list (e.g. a
/// race during a fresh pair that we don't ship for AutomationFired
/// but still want to handle gracefully).
pub(crate) async fn resolve_tv_name(daemon: &DaemonClient, tv_id: &str) -> String {
    match daemon.list_tvs().await {
        Ok(tvs) => tvs
            .into_iter()
            .find(|t| t.uuid == tv_id)
            .map(|t| t.name)
            .unwrap_or_else(|| tv_id.to_string()),
        Err(e) => {
            warn!(tv_id = %tv_id, error = %e, "automation-name lookup failed");
            tv_id.to_string()
        }
    }
}

/// Resolve the originating profile's display name from its UUID.
/// `profile_kind` selects which catalog to consult — `"system"` →
/// `Manager1.ListSystemProfiles`, `"user"` → `Session1.ListUserProfiles`.
/// On any lookup failure (catalog fetch errored, profile was deleted
/// between emit and resolve, session client unavailable), falls back
/// to the UUID's first 8 chars so the notification body still has
/// *something* identifying the profile.
pub(crate) async fn resolve_profile_name(
    daemon: &DaemonClient,
    session: &RwLock<Option<SessionClient>>,
    profile_kind: &str,
    profile_uuid: &str,
) -> String {
    let lookup = match profile_kind {
        "system" => daemon
            .list_system_profiles()
            .await
            .ok()
            .and_then(|profiles| profiles.into_iter().find(|(uuid, _)| uuid == profile_uuid))
            .map(|(_, name)| name),
        "user" => {
            let guard = session.read().await;
            match guard.as_ref() {
                Some(client) => client
                    .list_user_profiles()
                    .await
                    .ok()
                    .and_then(|profiles| {
                        profiles.into_iter().find(|(uuid, _)| uuid == profile_uuid)
                    })
                    .map(|(_, name)| name),
                None => None,
            }
        }
        _ => None,
    };
    lookup.unwrap_or_else(|| profile_uuid.chars().take(8).collect())
}

/// Format the notification summary + body from the stable wire
/// identifiers. Returns `(summary, body)` ready for
/// `Notifier::notify`.
///
/// Summary: "{tv_name}: {action_label}" — short, scannable, names
/// the affected TV first so multi-TV setups can tell at a glance
/// which one fired.
///
/// Body: "Triggered by {event_label} ({profile_kind} profile:
/// {profile_name})." — supplies the cause so the user can correlate
/// with what they were doing, plus names the profile so users with
/// multiple TVs on different profiles know which rule fired.
///
/// Unknown kinds (forward-compat with services that grew new
/// event/action variants this tray doesn't know about) fall through
/// to the raw snake_case identifier — still readable, just not
/// pretty-cased.
pub(crate) fn format_copy(
    tv_name: &str,
    event_kind: &str,
    action_kind: &str,
    profile_kind: &str,
    profile_name: &str,
) -> (String, String) {
    let action_label = action_label(action_kind);
    let event_label = event_label(event_kind);
    let summary = gettext("{0}: {1}")
        .replace("{0}", tv_name)
        .replace("{1}", &action_label);
    let body = gettext("Triggered by {0} ({1} profile: {2}).")
        .replace("{0}", &event_label)
        .replace("{1}", profile_kind)
        .replace("{2}", profile_name);
    (summary, body)
}

/// Translate the `SessionAction::Display` snake_case into a
/// user-visible label. Keys are stable; translators see one row per
/// action kind in the `.po` catalog.
fn action_label(action_kind: &str) -> String {
    match action_kind {
        "screen_off" => gettext("Turned screen off"),
        "screen_on" => gettext("Turned screen on"),
        "power_off" => gettext("Powered off"),
        "power_on" => gettext("Powered on"),
        "none" => gettext("No action"),
        other => other.to_string(),
    }
}

/// Translate the `SessionEvent::Display` / `SystemEvent::Display`
/// snake_case into a user-visible label. Same gettext-key convention
/// as [`action_label`]. Covers both user-profile event kinds (session
/// lock/unlock, idle, greeter) and system-profile lifecycle hooks
/// (system startup/shutdown/reboot/suspend/resume + all-sessions-end).
fn event_label(event_kind: &str) -> String {
    match event_kind {
        // User-profile events (Session1.AutomationFired)
        "session_locked" => gettext("session lock"),
        "session_unlocked" => gettext("session unlock"),
        "greeter_wake" => gettext("greeter wake"),
        "idle_timeout" => gettext("idle timeout"),
        "idle_resume" => gettext("idle resume"),
        // System-profile events (Manager1.AutomationFired)
        "on_startup" => gettext("system startup"),
        "on_shutdown" => gettext("system shutdown"),
        "on_reboot" => gettext("system reboot"),
        "on_sleep" => gettext("system suspend"),
        "on_wake" => gettext("system resume"),
        "on_no_session" => gettext("all sessions ended"),
        other => other.to_string(),
    }
}
