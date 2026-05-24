//! `automation_fired` notification copy + TV-name resolution.
//!
//! `Session1.AutomationFired` carries three stable snake_case
//! identifiers (`tv_id`, `event_kind`, `action_kind`). The tray
//! resolves the TV's friendly name client-side and translates the
//! event + action kinds via `gettext` lookup keys so the surfaced
//! notification reads as "<tv name>: <action> (<event>)" in the
//! user's locale.

use gettextrs::gettext;
use tisvlay_core::DaemonClient;
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

/// Format the notification summary + body from the stable wire
/// identifiers. Returns `(summary, body)` ready for
/// `Notifier::notify`.
///
/// Summary: "{tv_name}: {action_label}" — short, scannable, names
/// the affected TV first so multi-TV setups can tell at a glance
/// which one fired.
///
/// Body: "Triggered by {event_label}." — supplies the cause so the
/// user can correlate with what they were doing.
///
/// Unknown kinds (forward-compat with a session service that grew
/// new event/action variants this tray doesn't know about) fall
/// through to the raw snake_case identifier — still readable, just
/// not pretty-cased.
pub(crate) fn format_copy(tv_name: &str, event_kind: &str, action_kind: &str) -> (String, String) {
    let action_label = action_label(action_kind);
    let event_label = event_label(event_kind);
    let summary = gettext("{0}: {1}")
        .replace("{0}", tv_name)
        .replace("{1}", &action_label);
    let body = gettext("Triggered by {0}.").replace("{0}", &event_label);
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

/// Translate the `SessionEvent::Display` snake_case into a
/// user-visible label. Same gettext-key convention as
/// [`action_label`].
fn event_label(event_kind: &str) -> String {
    match event_kind {
        "session_locked" => gettext("session lock"),
        "session_unlocked" => gettext("session unlock"),
        "greeter_wake" => gettext("greeter wake"),
        "idle_timeout" => gettext("idle timeout"),
        "idle_resume" => gettext("idle resume"),
        other => other.to_string(),
    }
}
