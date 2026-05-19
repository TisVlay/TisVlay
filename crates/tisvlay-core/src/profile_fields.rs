//! Typed field-kind registry shared by the daemon-side
//! `SystemProfile1`, the session-side `UserProfile1`, and the GUI's
//! profile-pane field editor.
//!
//! One source of truth for each profile-field's wire name, D-Bus
//! property name, gettext label, and dispatch kind. `Bool` / `Text`
//! variants are placeholders — kept in the enum so adding the first
//! instance is a one-table-entry change rather than a structural
//! refactor.

/// Dispatch tag for one inheritable field.
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// ComboBox over a fixed option set (serde `rename_all = "snake_case"`).
    Enum { options: &'static [&'static str] },
    /// SpinBox over a bounded non-negative integer. `unit` is a
    /// translator-facing suffix appended to the displayed value.
    Number {
        min: i64,
        max: i64,
        step: i64,
        unit: &'static str,
    },
    /// Switch toggle. Placeholder — no in-tree field uses it yet.
    Bool,
    /// LineEdit. Placeholder — no in-tree field uses it yet.
    Text { max_len: usize },
}

/// One inheritable field's static metadata. Same shape on both the
/// daemon side and the session side; the catalog of fields differs
/// per profile kind ([`SYSTEM_FIELDS`] vs [`USER_FIELDS`]).
#[derive(Debug, Clone, Copy)]
pub struct FieldDef {
    /// PascalCase D-Bus property name (e.g. `"OnStartup"`).
    pub prop_name: &'static str,
    /// snake_case wire id matching `LocallySetFields` + `ResetField`.
    pub wire_name: &'static str,
    /// English msgid for the GUI label (fed through `gettext` / `@tr`
    /// at display time so the translator catalog stays authoritative).
    pub label_msgid: &'static str,
    /// Drives the GUI's row-component selection.
    pub kind: FieldKind,
}

impl FieldDef {
    /// Constructor for a `SessionAction`-typed enum field. Collapses
    /// the bulk of [`SYSTEM_FIELDS`] / [`USER_FIELDS`] entries to one
    /// line each.
    pub const fn session_action(
        prop_name: &'static str,
        wire_name: &'static str,
        label_msgid: &'static str,
    ) -> Self {
        Self {
            prop_name,
            wire_name,
            label_msgid,
            kind: FieldKind::Enum {
                options: SESSION_ACTIONS,
            },
        }
    }
}

/// `SessionAction` value set as serde wire strings (`rename_all =
/// "snake_case"`). Shared by every per-event hook on both sides.
pub const SESSION_ACTIONS: &[&str] = &["power_on", "power_off", "screen_on", "screen_off", "none"];

/// `AutomateWhen` value set — only used by the system side's
/// `automate_when` field.
pub const AUTOMATE_WHEN_OPTIONS: &[&str] = &["always", "on_primary", "off_primary"];

/// System-profile field catalog. Order is the display order in the
/// Profiles pane.
pub const SYSTEM_FIELDS: &[FieldDef] = &[
    FieldDef::session_action("OnStartup", "on_startup", "On startup"),
    FieldDef::session_action("OnShutdown", "on_shutdown", "On shutdown"),
    FieldDef::session_action("OnReboot", "on_reboot", "On reboot"),
    FieldDef::session_action("OnNoSession", "on_no_session", "On no session"),
    FieldDef::session_action("OnSleep", "on_sleep", "On sleep"),
    FieldDef::session_action("OnWake", "on_wake", "On wake"),
    FieldDef {
        prop_name: "AutomateWhen",
        wire_name: "automate_when",
        label_msgid: "Automate when",
        kind: FieldKind::Enum {
            options: AUTOMATE_WHEN_OPTIONS,
        },
    },
];

/// User-profile field catalog. Order is the display order in the
/// Profiles pane.
///
/// `IdleTimeoutSeconds` caps at 86400 seconds — anything above falls
/// in the "effectively disabled" category, which is what `0` is for.
/// The body stores `Option<u64>`; D-Bus wire form is `u` (u32).
pub const USER_FIELDS: &[FieldDef] = &[
    FieldDef::session_action("OnSessionLock", "on_session_lock", "On session lock"),
    FieldDef::session_action("OnSessionUnlock", "on_session_unlock", "On session unlock"),
    FieldDef::session_action("OnGreeterWake", "on_greeter_wake", "On greeter wake"),
    FieldDef::session_action("OnIdleTimeout", "on_idle_timeout", "On idle timeout"),
    FieldDef::session_action("OnIdleResume", "on_idle_resume", "On idle resume"),
    FieldDef {
        prop_name: "IdleTimeoutSeconds",
        wire_name: "idle_timeout_seconds",
        label_msgid: "Idle timeout (seconds)",
        kind: FieldKind::Number {
            min: 0,
            max: 86_400,
            step: 1,
            unit: "seconds",
        },
    },
];

/// Find a field by snake_case wire name.
pub fn find_in(table: &'static [FieldDef], wire_name: &str) -> Option<&'static FieldDef> {
    table.iter().find(|f| f.wire_name == wire_name)
}

/// Find a field by PascalCase D-Bus property name (used by the GUI's
/// `PropertiesChanged` listener).
pub fn find_in_by_prop(table: &'static [FieldDef], prop_name: &str) -> Option<&'static FieldDef> {
    table.iter().find(|f| f.prop_name == prop_name)
}
