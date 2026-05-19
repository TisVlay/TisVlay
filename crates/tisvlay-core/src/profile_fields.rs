//! Typed field-kind registry shared by the daemon-side
//! `SystemProfile1`, the session-side `UserProfile1`, and the GUI's
//! profile-pane field editor.
//!
//! One source of truth for each profile-field's wire name, D-Bus
//! property name, gettext label, and dispatch kind. The GUI walks
//! the tables to build its per-row Slint model; the daemon and
//! session sides import the same tables to validate field names
//! and dispatch typed writes (the [`profile_field_props!`] macro
//! also reads from them indirectly via the per-interface invocation).
//!
//! The kind tag drives the GUI's row-component dispatch: `Enum` →
//! ComboBox, `Number` → SpinBox, `Bool` → Switch, `Text` → LineEdit.
//! `Bool` and `Text` variants are placeholders today — no in-tree
//! field uses them — kept in the enum so adding the first instance
//! is a one-table-entry change rather than a structural refactor.
//!
//! [`profile_field_props!`]: crate::profile_field_props

/// Dispatch tag for one inheritable field. Drives both the GUI's
/// row-component selection and the daemon/session-side parsing of
/// values landing through `Properties.Set`.
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// Enumerated string value drawn from a fixed option set.
    /// Renders as a ComboBox.
    Enum {
        /// Wire-form (serde `rename_all = "snake_case"`) values the
        /// dropdown offers. Inheriting is signaled via
        /// `LocallySetFields` + the row's reset glyph, not a
        /// dropdown option.
        options: &'static [&'static str],
    },
    /// Non-negative integer with explicit bounds. Renders as a
    /// SpinBox. `unit` is a translator-facing suffix (`"seconds"`,
    /// `"minutes"`, …) appended to the displayed value.
    Number {
        min: i64,
        max: i64,
        step: i64,
        unit: &'static str,
    },
    /// Boolean toggle. Renders as a Switch. Placeholder — no
    /// in-tree field uses it yet.
    Bool,
    /// Free-form text bounded by `max_len`. Renders as a LineEdit.
    /// Placeholder — no in-tree field uses it yet.
    Text { max_len: usize },
}

/// One inheritable field's static metadata. Same shape on both the
/// daemon side and the session side; the catalog of fields differs
/// per profile kind ([`SYSTEM_FIELDS`] vs [`USER_FIELDS`]).
#[derive(Debug, Clone, Copy)]
pub struct FieldDef {
    /// D-Bus property name (PascalCase) — first argument to
    /// `Properties.Set`. Example: `"OnStartup"`.
    pub prop_name: &'static str,
    /// Wire id (snake_case) — matches `LocallySetFields` entries and
    /// the `ResetField` argument. Example: `"on_startup"`.
    pub wire_name: &'static str,
    /// English msgid for the GUI label, fed through `gettext` / `@tr`
    /// at display time so the translator catalog is the source of
    /// truth.
    pub label_msgid: &'static str,
    /// Dispatch kind — drives the GUI's row-component selection and
    /// the daemon's typed-value parsing.
    pub kind: FieldKind,
}

impl FieldDef {
    /// Concrete option set for an [`FieldKind::Enum`] field. Returns
    /// `None` for other kinds — caller decides whether that's a
    /// validation failure or a silent skip.
    pub fn enum_options(&self) -> Option<&'static [&'static str]> {
        match self.kind {
            FieldKind::Enum { options } => Some(options),
            _ => None,
        }
    }
}

/// `SessionAction` value set as serde wire strings (`rename_all =
/// "snake_case"`). Shared by every per-event hook on both the system
/// side (startup/shutdown/reboot/no-session/sleep/wake) and the user
/// side (lock/unlock/greeter-wake/idle-timeout/idle-resume).
pub const SESSION_ACTIONS: &[&str] = &["power_on", "power_off", "screen_on", "screen_off", "none"];

/// `AutomateWhen` value set. Only used on the system side's
/// `automate_when` field.
pub const AUTOMATE_WHEN_OPTIONS: &[&str] = &["always", "on_primary", "off_primary"];

/// System-profile field catalog. Order is the display order in the
/// Profiles pane.
pub const SYSTEM_FIELDS: &[FieldDef] = &[
    FieldDef {
        prop_name: "OnStartup",
        wire_name: "on_startup",
        label_msgid: "On startup",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnShutdown",
        wire_name: "on_shutdown",
        label_msgid: "On shutdown",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnReboot",
        wire_name: "on_reboot",
        label_msgid: "On reboot",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnNoSession",
        wire_name: "on_no_session",
        label_msgid: "On no session",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnSleep",
        wire_name: "on_sleep",
        label_msgid: "On sleep",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnWake",
        wire_name: "on_wake",
        label_msgid: "On wake",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
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
/// `IdleTimeoutSeconds` rides as `Number { min: 0, max: 86400 }` —
/// 24 hours is the upper bound that's still meaningful for an idle
/// timer (anything above falls in the "effectively disabled"
/// category, which is what `0` is for). The daemon stores the body
/// as `Option<u64>`; the D-Bus wire form is `u` (u32). The 86400-cap
/// is therefore well inside both wire and storage ranges.
pub const USER_FIELDS: &[FieldDef] = &[
    FieldDef {
        prop_name: "OnSessionLock",
        wire_name: "on_session_lock",
        label_msgid: "On session lock",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnSessionUnlock",
        wire_name: "on_session_unlock",
        label_msgid: "On session unlock",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnGreeterWake",
        wire_name: "on_greeter_wake",
        label_msgid: "On greeter wake",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnIdleTimeout",
        wire_name: "on_idle_timeout",
        label_msgid: "On idle timeout",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
    FieldDef {
        prop_name: "OnIdleResume",
        wire_name: "on_idle_resume",
        label_msgid: "On idle resume",
        kind: FieldKind::Enum {
            options: SESSION_ACTIONS,
        },
    },
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

/// Find a field def by wire-name in either catalog. Returns `None`
/// if the name doesn't match a registered field — caller decides
/// whether that's a validation failure or a silent skip.
pub fn find_in(table: &'static [FieldDef], wire_name: &str) -> Option<&'static FieldDef> {
    table.iter().find(|f| f.wire_name == wire_name)
}

/// Same as [`find_in`] but matches against the PascalCase D-Bus
/// property name. Used by the GUI's `PropertiesChanged` listener,
/// which receives prop names in their wire-PascalCase form.
pub fn find_in_by_prop(table: &'static [FieldDef], prop_name: &str) -> Option<&'static FieldDef> {
    table.iter().find(|f| f.prop_name == prop_name)
}
