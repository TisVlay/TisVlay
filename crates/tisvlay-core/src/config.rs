//! Profile vocabulary: the system and user profile bodies, the actions
//! their events map to, how a body resolves over its defaults, and the
//! reserved default profiles. The daemon persists system profiles in its
//! store and the session service persists user profiles in its own; this
//! module is the shape both share with clients.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// =============================================================================
// Actions
// =============================================================================

/// Actions the session service or daemon can take in response to events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    /// Do nothing.
    #[default]
    None,
    /// Turn screen off (OLED-safe blank).
    ScreenOff,
    /// Turn screen on.
    ScreenOn,
    /// Power off the TV.
    PowerOff,
    /// Power on the TV (Wake-on-LAN).
    PowerOn,
}

/// One event's mapping: an action, optionally carrying modifiers.
/// Serializes as the bare action string (`"screen_on"`) or as a table
/// (`{ action = "screen_on", restore_input = true }`) — two shapes of
/// one grammar, round-tripped in whichever form was written.
///
/// The `while_remote` modifier is the remote-session gate: `false`
/// (the bare-token default) suppresses this event's action while the
/// TV is armed by a remote session; `true` lets it fire regardless.
/// The resolved floor splits by event direction: suppression protects
/// against unwanted wakes, so wake-ish stock events keep the gated
/// default (an unconfigured profile stays dark for an empty room),
/// while off-going stock events (blanks and power-offs) carry an
/// explicit `true` — a blank is always safe to let through. Because it
/// can only suppress, the gate can never originate an action an unarmed
/// TV would not take. It is inert on `none` rows (nothing to suppress).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EventAction {
    /// Bare action, no modifiers.
    Plain(SessionAction),
    /// Action with modifiers.
    With {
        /// The action to take.
        action: SessionAction,
        /// Land the TV on its configured `primary_input` once the
        /// action completes. Meaningful for the on-ish actions on a
        /// backend advertising the INPUT capability; inert otherwise.
        #[serde(default)]
        restore_input: bool,
        /// Whether this event's action still fires while the TV is
        /// armed by a remote session. `false` suppresses it. Defaults
        /// to `false` so the bare-token form (and any table omitting
        /// the field) is gated — one suppress-by-default rule.
        #[serde(default = "while_remote_default")]
        while_remote: bool,
    },
}

/// The default for [`EventAction`]'s `while_remote` field: gated
/// (suppressed while armed). A named fn (not an inline closure) so
/// serde's `default = "…"` attribute can name it. Kept in sync with the
/// [`Plain`](EventAction::Plain) arm of [`EventAction::while_remote`] so
/// a bare token and a field-omitting table gate identically.
fn while_remote_default() -> bool {
    false
}

impl EventAction {
    /// Table-form constructor for restore-on mappings (the stock
    /// session-start defaults). Gate defaults to gated (suppress while
    /// armed) — the suppress-by-default floor.
    pub fn restoring(action: SessionAction) -> Self {
        Self::With {
            action,
            restore_input: true,
            while_remote: while_remote_default(),
        }
    }

    /// Table-form constructor for a floor-exempt mapping — an action
    /// that still fires while the TV is armed. Off-going stock events
    /// (blanks and power-offs) use this: a blank is always safe to let
    /// through, and the machine going away should turn the TV off
    /// regardless.
    pub fn ungated(action: SessionAction) -> Self {
        Self::With {
            action,
            restore_input: false,
            while_remote: true,
        }
    }

    /// The action this row takes, with any modifiers dropped.
    pub fn action(self) -> SessionAction {
        match self {
            Self::Plain(action) | Self::With { action, .. } => action,
        }
    }

    /// Whether the TV should land on its configured `primary_input` after the
    /// action. Only the modifier form can ask for it.
    pub fn restore_input(self) -> bool {
        match self {
            Self::Plain(_) => false,
            Self::With { restore_input, .. } => restore_input,
        }
    }

    /// Whether this event's action fires while the TV is armed by a
    /// remote session. The bare (`Plain`) form is always gated
    /// (`false`), matching `while_remote_default` — a bare token is
    /// suppressed while armed; firing anyway is opt-in through the
    /// table form.
    pub fn while_remote(self) -> bool {
        match self {
            Self::Plain(_) => while_remote_default(),
            Self::With { while_remote, .. } => while_remote,
        }
    }
}

impl From<SessionAction> for EventAction {
    fn from(action: SessionAction) -> Self {
        Self::Plain(action)
    }
}

/// Action-only comparison — the modifier is orthogonal to "which verb".
impl PartialEq<SessionAction> for EventAction {
    fn eq(&self, other: &SessionAction) -> bool {
        self.action() == *other
    }
}

impl Default for EventAction {
    fn default() -> Self {
        Self::Plain(SessionAction::None)
    }
}

// =============================================================================
// AutomateWhen — input-aware automation gating
// =============================================================================

/// Controls when automation fires relative to the TV's current input.
///
/// Combined with the per-TV `primary_input` and the global `managed` flag
/// to derive the TV's effective automated state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomateWhen {
    /// Always apply automation regardless of current input.
    #[default]
    Always,
    /// Only apply automation when the TV is on the primary input.
    OnPrimary,
    /// Only apply automation when the TV is NOT on the primary input.
    OffPrimary,
}

impl AutomateWhen {
    /// Evaluate the automation gate.
    ///
    /// Returns `true` if automation should fire given the current input state.
    /// When either `primary_input` or `current_input` is unknown (`None`),
    /// returns `true` (benefit of the doubt — can't check input in standby).
    pub fn should_automate(self, primary_input: Option<&str>, current_input: Option<&str>) -> bool {
        match self {
            Self::Always => true,
            Self::OnPrimary => match (primary_input, current_input) {
                (None, _) | (_, None) => true,
                (Some(primary), Some(current)) => current == primary,
            },
            Self::OffPrimary => match (primary_input, current_input) {
                (None, _) | (_, None) => true,
                (Some(primary), Some(current)) => current != primary,
            },
        }
    }
}

impl std::str::FromStr for AutomateWhen {
    type Err = String;

    /// Inverse of [`Display`](Self#impl-Display-for-AutomateWhen); the
    /// round-trip test below pins both against the serde shape.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "always" => Ok(Self::Always),
            "on_primary" => Ok(Self::OnPrimary),
            "off_primary" => Ok(Self::OffPrimary),
            other => Err(format!("unknown automate_when: {other}")),
        }
    }
}

impl std::fmt::Display for AutomateWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Always => write!(f, "always"),
            Self::OnPrimary => write!(f, "on_primary"),
            Self::OffPrimary => write!(f, "off_primary"),
        }
    }
}

// =============================================================================
// Profile plumbing macro
// =============================================================================

/// Generate the mechanical per-field plumbing shared by both profile
/// types from a single inheritable-field list:
///
/// - `$config::full_default()` / `merge_with_base()` / `resolve()` — the
///   `Option<T>` inheritance chain (`Some(default)` / `or` / `unwrap_or`).
/// - `pub const $fields: &[&str]` — the field names in declaration
///   order, the single source of truth the GUI field tables are
///   validated against (see `tisvlay-gui`'s `*_field_table_*` tests).
///
/// `name` is handled explicitly (it's identity-adjacent metadata, not an
/// inheritable field) so it never appears in the list. The `Resolved`
/// struct, and its `Default`, stay hand-written — adding a field still
/// requires touching those, so the field set can't silently diverge
/// from the resolved defaults.
macro_rules! profile_plumbing {
    (
        config: $config:ident,
        resolved: $resolved:ident,
        fields_const: $fields:ident,
        fields: [$($field:ident),+ $(,)?],
    ) => {
        /// Inheritable field names, in declaration order.
        pub const $fields: &[&str] = &[$(stringify!($field)),+];

        impl $config {
            /// Create a profile with every field set to the built-in
            /// default. The `name` is left empty for the caller to set.
            pub fn full_default() -> Self {
                let d = $resolved::default();
                Self {
                    name: String::new(),
                    $($field: Some(d.$field),)+
                }
            }

            /// Merge with a base: `None` fields fall through to `base`,
            /// `Some` fields win. `name` is preserved from `self`.
            #[must_use]
            pub fn merge_with_base(&self, base: &$config) -> $config {
                $config {
                    name: self.name.clone(),
                    $($field: self.$field.or(base.$field),)+
                }
            }

            /// Resolve to concrete values, filling `None` fields from
            /// `$resolved::default()`.
            pub fn resolve(&self) -> $resolved {
                let d = $resolved::default();
                $resolved {
                    $($field: self.$field.unwrap_or(d.$field),)+
                }
            }
        }
    };
}

// =============================================================================
// SystemProfileConfig (with Option<T> fields for merge support)
// =============================================================================

/// System profile configuration for daemon lifecycle events.
///
/// Fields are `Option<T>` to support layered inheritance:
/// per-TV system profile -> default system profile -> hard-coded defaults.
///
/// Use [`SystemProfileConfig::resolve()`] or [`SystemProfileConfig::merge_with_base()`]
/// to produce a [`ResolvedSystemProfile`] with concrete values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemProfileConfig {
    /// Friendly name shown in the GUI / CLI. Enforced unique across
    /// the system catalog. Profile identity itself is the
    /// containing-map's UUID key — renaming is a name-only edit and
    /// doesn't touch references (TVs reference profiles by UUID).
    pub name: String,

    /// Action on system startup/boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_startup: Option<EventAction>,

    /// Action when the system powers off or halts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_shutdown: Option<EventAction>,

    /// Action when the system reboots (including kexec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_reboot: Option<EventAction>,

    /// Action on system sleep/suspend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_sleep: Option<EventAction>,

    /// Action on system wake/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wake: Option<EventAction>,

    /// Action when the matched host display connector blanks (DPMS off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_dpms_off: Option<EventAction>,

    /// Action when the matched host display connector wakes (DPMS on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_dpms_on: Option<EventAction>,

    /// Greeter idle timeout in seconds (0 to disable). Drives the
    /// `tisvlay-greeter` idle client at the login screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<u64>,

    /// Action when the greeter goes idle (after `idle_timeout_seconds`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_timeout: Option<EventAction>,

    /// Action when greeter activity resumes after idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_resume: Option<EventAction>,

    /// When to apply automation relative to the TV's current input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automate_when: Option<AutomateWhen>,
}

profile_plumbing! {
    config: SystemProfileConfig,
    resolved: ResolvedSystemProfile,
    fields_const: SYSTEM_PROFILE_FIELDS,
    fields: [
        on_startup,
        on_shutdown,
        on_reboot,
        on_sleep,
        on_wake,
        on_dpms_off,
        on_dpms_on,
        idle_timeout_seconds,
        on_idle_timeout,
        on_idle_resume,
        automate_when,
    ],
}

// =============================================================================
// ResolvedSystemProfile (fully resolved, no Option fields)
// =============================================================================

/// A fully resolved system profile with no optional fields.
///
/// This is the result of merging a [`SystemProfileConfig`] chain
/// (per-TV system profile -> default system profile -> hard-coded defaults).
/// The daemon uses this type at event dispatch time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSystemProfile {
    /// Action on system startup/boot.
    pub on_startup: EventAction,
    /// Action when the system powers off or halts.
    pub on_shutdown: EventAction,
    /// Action when the system reboots (including kexec).
    pub on_reboot: EventAction,
    /// Action on system sleep/suspend.
    pub on_sleep: EventAction,
    /// Action on system wake/resume.
    pub on_wake: EventAction,
    /// Action when the matched host display connector blanks (DPMS off).
    pub on_dpms_off: EventAction,
    /// Action when the matched host display connector wakes (DPMS on).
    pub on_dpms_on: EventAction,
    /// Greeter idle timeout in seconds (0 to disable).
    pub idle_timeout_seconds: u64,
    /// Action when the greeter goes idle.
    pub on_idle_timeout: EventAction,
    /// Action when greeter activity resumes after idle.
    pub on_idle_resume: EventAction,
    /// When to apply automation relative to the TV's current input.
    pub automate_when: AutomateWhen,
}

impl Default for ResolvedSystemProfile {
    fn default() -> Self {
        Self {
            // The session-start wakes restore the primary input by
            // default (a CEC or WoL wake can land on another port);
            // every other mapping stays input-neutral.
            //
            // Floor gate splits by direction: off-going events fire even
            // while armed (a blank is always safe to let through, and the
            // machine going away should still turn the panel off for the
            // empty room), so the DPMS-off blank and the idle-timeout
            // blank join sleep/shutdown/reboot as ungated. Wake-ish events
            // stay gated (startup so a boot while remoted leaves the TV
            // dark; the wake, DPMS-on, and idle-resume mirrors so a wake
            // for an empty room is suppressed).
            on_startup: EventAction::restoring(SessionAction::PowerOn),
            on_shutdown: EventAction::ungated(SessionAction::PowerOff),
            on_reboot: EventAction::ungated(SessionAction::None),
            on_sleep: EventAction::ungated(SessionAction::PowerOff),
            on_wake: EventAction::restoring(SessionAction::PowerOn),
            on_dpms_off: EventAction::ungated(SessionAction::ScreenOff),
            on_dpms_on: SessionAction::ScreenOn.into(),
            // Off by default: the greeter idle timer is opt-in (set a non-zero
            // timeout, e.g. via the NixOS `greeter.enable` option). The DPMS
            // mirror above is the only default-on screen automation.
            idle_timeout_seconds: 0,
            on_idle_timeout: EventAction::ungated(SessionAction::ScreenOff),
            on_idle_resume: SessionAction::ScreenOn.into(),
            automate_when: AutomateWhen::Always,
        }
    }
}

// =============================================================================
// UserProfileConfig (with Option<T> fields for merge support)
// =============================================================================

/// Profile configuration for session behavior.
///
/// Fields are `Option<T>` to support layered inheritance:
/// user profile -> system default profile -> hard-coded defaults.
///
/// Use [`UserProfileConfig::resolve()`] or [`UserProfileConfig::merge_with_base()`]
/// to produce a [`ResolvedUserProfile`] with concrete values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfileConfig {
    /// Friendly name shown in the GUI / CLI. Enforced unique across
    /// the user catalog. Profile identity itself is the containing-map's
    /// UUID key — renaming is a name-only edit and doesn't touch
    /// references (TVs reference profiles by UUID).
    pub name: String,

    /// Action on session lock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_session_lock: Option<EventAction>,

    /// Action on session unlock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_session_unlock: Option<EventAction>,

    /// Action when user interacts with the lock screen greeter (before unlocking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_lock_wake: Option<EventAction>,

    /// Idle timeout in seconds while the screen is locked (0 to
    /// disable) — how long a bumped-but-not-unlocked lock screen stays
    /// awake before `on_idle_timeout` re-blanks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_idle_timeout_seconds: Option<u64>,

    /// Idle timeout in seconds while unlocked (0 to disable) — the
    /// in-session idle threshold for `on_idle_timeout`/`on_idle_resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlocked_idle_timeout_seconds: Option<u64>,

    /// Action on idle timeout (either state's timer; each timer only
    /// fires in its own lock state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_timeout: Option<EventAction>,

    /// Action when activity resumes after an unlocked idle timeout.
    /// Activity while locked dispatches `on_lock_wake` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_resume: Option<EventAction>,

    /// Action when a remote session connects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_remote_connect: Option<EventAction>,

    /// Action when the last remote session disconnects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_remote_disconnect: Option<EventAction>,

    /// Action on the first local input after a remote session ends —
    /// wakes the panel that the disconnect blank left dark for the
    /// returning person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_local_return: Option<EventAction>,

    /// Whether an unattributed screen-capture signal arms the remote
    /// session. Attributed sources (remote apps, hooks, logind) always
    /// arm; this decides only the bare screencast case (OBS, browser
    /// tab-share). Default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_screencast_arms: Option<bool>,
}

profile_plumbing! {
    config: UserProfileConfig,
    resolved: ResolvedUserProfile,
    fields_const: USER_PROFILE_FIELDS,
    fields: [
        on_session_lock,
        on_session_unlock,
        on_lock_wake,
        locked_idle_timeout_seconds,
        unlocked_idle_timeout_seconds,
        on_idle_timeout,
        on_idle_resume,
        on_remote_connect,
        on_remote_disconnect,
        on_local_return,
        remote_screencast_arms,
    ],
}

// =============================================================================
// ResolvedUserProfile (fully resolved, no Option fields)
// =============================================================================

/// A fully resolved profile with no optional fields.
///
/// This is the result of merging a [`UserProfileConfig`] chain
/// (user profile -> system default -> hard-coded defaults).
/// Consumers (session monitor, idle detector) use this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedUserProfile {
    /// Action on session lock event.
    pub on_session_lock: EventAction,
    /// Action on session unlock event.
    pub on_session_unlock: EventAction,
    /// Action when user interacts with the lock screen greeter.
    pub on_lock_wake: EventAction,
    /// Idle timeout in seconds while locked (0 to disable).
    pub locked_idle_timeout_seconds: u64,
    /// Idle timeout in seconds while unlocked (0 to disable).
    pub unlocked_idle_timeout_seconds: u64,
    /// Action on idle timeout.
    pub on_idle_timeout: EventAction,
    /// Action when activity resumes after an unlocked idle timeout.
    pub on_idle_resume: EventAction,
    /// Action when a remote session connects.
    pub on_remote_connect: EventAction,
    /// Action when the last remote session disconnects.
    pub on_remote_disconnect: EventAction,
    /// Action on the first local input after a remote session ends.
    pub on_local_return: EventAction,
    /// Whether an unattributed screen-capture signal arms.
    pub remote_screencast_arms: bool,
}

impl Default for ResolvedUserProfile {
    fn default() -> Self {
        Self {
            // Floor gate splits by direction: the off-going blanks (lock,
            // idle-timeout) are ungated — a blank is always safe to let
            // through even for an armed TV — while the wake-ish events
            // (unlock, lock-wake, idle-resume) stay gated so a wake for an
            // empty room is suppressed. Unlock and local-return restore the
            // primary input (a user is arriving at the panel — a fresh
            // desktop session, or a return from a remote one that may have
            // left the TV's input elsewhere); the other mappings stay
            // input-neutral.
            on_session_lock: EventAction::ungated(SessionAction::ScreenOff),
            on_session_unlock: EventAction::restoring(SessionAction::ScreenOn),
            on_lock_wake: SessionAction::ScreenOn.into(),
            locked_idle_timeout_seconds: 0,
            unlocked_idle_timeout_seconds: 0,
            on_idle_timeout: EventAction::ungated(SessionAction::ScreenOff),
            on_idle_resume: SessionAction::ScreenOn.into(),
            // Both remote edges blank the panel at the floor: the room is
            // empty when a remote session arms, and it stays dark when the
            // session disarms. A returning person wakes it through
            // on_local_return (the first local input after the disconnect);
            // the unlock/lock-wake/idle-resume paths cover only the locked
            // or already-idled cases. Detection, not policy, is the opt-in —
            // nothing arms until a detector or hook fires, so the feature
            // stays inert on a stock desktop. The two edges ignore the gate
            // (they fire on the arm/disarm edge itself), so they carry the
            // plain default; on_local_return stays gated but wakes restoring
            // (it fires un-armed in the common case); capture arming is
            // still off until a profile turns it on.
            on_remote_connect: SessionAction::ScreenOff.into(),
            on_remote_disconnect: SessionAction::ScreenOff.into(),
            on_local_return: EventAction::restoring(SessionAction::ScreenOn),
            remote_screencast_arms: false,
        }
    }
}

// =============================================================================
// Reserved-default profile identities
// =============================================================================

/// Reserved UUID for the system-side `default` profile.
///
/// Wire-stable identity for the inheritance ancestor every other system
/// profile falls back to. The displayed label is the literal `"default"`
/// at the daemon boundary; clients are free to render it through
/// `gettext("default")` for localized display without the daemon ever
/// seeing the localized string.
pub const DEFAULT_SYSTEM_PROFILE_UUID: Uuid =
    Uuid::from_u128(0x00000000_0000_0000_0000_000000005159);

/// Reserved UUID for the user-side `default` profile. Mirror of
/// [`DEFAULT_SYSTEM_PROFILE_UUID`] on the user-config side.
pub const DEFAULT_USER_PROFILE_UUID: Uuid = Uuid::from_u128(0x00000000_0000_0000_0000_00000000515A);

/// Reserved display label for the system + user default profiles. The
/// daemon enforces this on the body's `name` field at load time
/// regardless of what's on disk under the default UUID; clients are
/// free to render it through `gettext()` for localized display
/// without the daemon ever seeing the localized string.
pub const DEFAULT_PROFILE_NAME: &str = "default";

// =============================================================================
// Config sections
// =============================================================================

/// User-level configuration section.
///
/// Contains user profiles that define session event behavior.
/// Located under `[user]` in the TOML config.
///
/// In the system config, the reserved [`DEFAULT_USER_PROFILE_UUID`] entry
/// is the admin baseline inherited by users who have not created their
/// own config file. In the user config, user-defined profiles
/// override/extend the admin baseline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserSection {
    /// User profiles keyed by stable UUID.
    #[serde(default)]
    pub profiles: BTreeMap<Uuid, UserProfileConfig>,
}

impl UserSection {
    /// Find a profile entry by its display name. Returns the UUID + body
    /// of the first match.
    pub fn find_profile_by_name(&self, name: &str) -> Option<(Uuid, &UserProfileConfig)> {
        self.profiles
            .iter()
            .find(|(_, body)| body.name == name)
            .map(|(uuid, body)| (*uuid, body))
    }

    /// Materialize the reserved-default profile if missing, then force
    /// its `name` field to [`DEFAULT_PROFILE_NAME`]. Idempotent.
    pub fn ensure_default(&mut self) {
        self.profiles
            .entry(DEFAULT_USER_PROFILE_UUID)
            .or_insert_with(UserProfileConfig::full_default);
        if let Some(body) = self.profiles.get_mut(&DEFAULT_USER_PROFILE_UUID) {
            body.name = DEFAULT_PROFILE_NAME.to_string();
        }
    }
}

// =============================================================================
// UserTvProfileAssignment
// =============================================================================

/// Per-TV user profile assignment.
///
/// Stored on `UserTvConfig` (in the user config's `[tv.<id>]` entries).
/// `user` is the UUID of the user profile governing this TV's session
/// behavior; defaults to [`DEFAULT_USER_PROFILE_UUID`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTvProfileAssignment {
    /// UUID of the user profile to use. Defaults to
    /// [`DEFAULT_USER_PROFILE_UUID`].
    #[serde(default = "default_user_profile_uuid")]
    pub user: Uuid,
}

impl Default for UserTvProfileAssignment {
    fn default() -> Self {
        Self {
            user: DEFAULT_USER_PROFILE_UUID,
        }
    }
}

fn default_user_profile_uuid() -> Uuid {
    DEFAULT_USER_PROFILE_UUID
}

// =============================================================================
// UserConfig
// =============================================================================

/// User-level configuration (session service-owned).
///
/// Contains profile definitions and per-TV profile assignments.
/// This file is optional — if absent, the session service inherits
/// the system default profile for all TVs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    /// User-defined profiles.
    ///
    /// Contains `[user.profiles.*]` section. The name "default" is reserved:
    /// it applies to any TV without an explicit profile assignment. Fields
    /// not specified inherit from the system default profile, NOT from
    /// hard-coded defaults.
    #[serde(default)]
    pub user: UserSection,

    /// Per-TV profile assignments. Uses `[[tv]]` array-of-tables syntax.
    #[serde(default)]
    pub tv: Vec<UserTvConfig>,
}

/// User's per-TV settings.
///
/// All fields except `uuid` have `#[serde(default)]` so that when parsing
/// a single-file config (where `[[tv]]` entries contain system fields like
/// `name`, `vendor`, etc.), unknown fields are silently ignored and missing
/// user-specific fields get defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTvConfig {
    /// The TV's UUID.
    /// Stored as canonical hyphenated string to keep the user config
    /// readable and tolerant of hand-edits.
    pub uuid: String,

    /// Profile assignment for this TV.
    #[serde(default)]
    pub profile: UserTvProfileAssignment,
}

impl UserConfig {
    /// Load user configuration from the default XDG path.
    ///
    /// Returns `Ok(None)` if the file doesn't exist (user hasn't customized yet).
    pub fn load() -> Result<Option<Self>> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    /// Load user configuration from a specific path.
    ///
    /// Returns `Ok(None)` if the file doesn't exist.
    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(Some(config))
    }

    /// Save user configuration to the default XDG path.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    /// Save user configuration to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content =
            toml::to_string_pretty(self).map_err(|e| Error::Io(std::io::Error::other(e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Get the user config file path.
    ///
    /// Uses XDG directories: `~/.config/tisvlay/session.toml`. Each
    /// peer service owns its own file in the user domain per the
    /// per-peer file naming convention.
    pub fn config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "tisvlay").ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            ))
        })?;
        Ok(dirs.config_dir().join("session.toml"))
    }

    /// Resolve the effective profile for a TV.
    ///
    /// Resolution chain:
    /// 1. User's per-TV profile assignment -> user's profile body (by UUID)
    /// 2. User's [`DEFAULT_USER_PROFILE_UUID`] profile body
    /// 3. System default profile (passed in)
    /// 4. Hard-coded defaults
    ///
    /// At each level, individual fields merge (user overrides system).
    pub fn resolve_user_profile(
        &self,
        tv_id: &str,
        system_default: &UserProfileConfig,
    ) -> ResolvedUserProfile {
        // 1. Find which profile UUID applies to this TV.
        let profile_uuid = self
            .tv
            .iter()
            .find(|t| t.uuid == tv_id)
            .map(|t| t.profile.user)
            .unwrap_or(DEFAULT_USER_PROFILE_UUID);

        // 2. Get the user's profile body by UUID (may be partial).
        let user_profile = self.user.profiles.get(&profile_uuid);

        // 3. Get the user's default profile (for field inheritance within user config)
        let user_default = if profile_uuid != DEFAULT_USER_PROFILE_UUID {
            self.user.profiles.get(&DEFAULT_USER_PROFILE_UUID)
        } else {
            None
        };

        // 4. Build the merge chain: user profile -> user default -> system default
        let merged = match (user_profile, user_default) {
            (Some(profile), Some(default)) => {
                // User profile -> user default -> system default
                profile
                    .merge_with_base(default)
                    .merge_with_base(system_default)
            }
            (Some(profile), None) => {
                // User profile -> system default
                profile.merge_with_base(system_default)
            }
            (None, Some(default)) => {
                // Profile UUID not found in user config — fall through to user's default
                // before reaching the system default, so user-level overrides are honored.
                default.merge_with_base(system_default)
            }
            (None, None) => {
                // No user profile or user default — use system default directly
                system_default.clone()
            }
        };

        // 5. Resolve any remaining Nones to hard-coded defaults
        merged.resolve()
    }

    /// Get the profile UUID assigned to a TV.
    ///
    /// Returns [`DEFAULT_USER_PROFILE_UUID`] if no explicit assignment exists.
    pub fn get_tv_user_profile(&self, tv_id: &str) -> Uuid {
        self.tv
            .iter()
            .find(|t| t.uuid == tv_id)
            .map(|t| t.profile.user)
            .unwrap_or(DEFAULT_USER_PROFILE_UUID)
    }

    /// Assign a profile to a TV by UUID.
    ///
    /// If an entry for this TV already exists, updates it. Otherwise appends a new entry.
    pub fn set_tv_user_profile(&mut self, tv_id: impl Into<String>, profile_uuid: Uuid) {
        let tv_id = tv_id.into();
        if let Some(entry) = self.tv.iter_mut().find(|t| t.uuid == tv_id) {
            entry.profile.user = profile_uuid;
        } else {
            self.tv.push(UserTvConfig {
                uuid: tv_id,
                profile: UserTvProfileAssignment { user: profile_uuid },
            });
        }
    }
}

/// Resolve a path to absolute form by prepending the current working
/// directory if it isn't already absolute. Used by daemon + session
/// at startup so the `ConfigPath` D-Bus property is always absolute,
/// regardless of how the binary was invoked. Falls back to the
/// original path if `current_dir()` itself fails (e.g., the CWD has
/// been deleted) — a relative path is still better than panicking.
pub fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn event_action_while_remote_gate_serde_shapes() {
        // The bare token round-trips as `Plain` and reads `while_remote
        // = false` (gated) — suppress-by-default, one rule everywhere.
        let plain: EventAction = serde_json::from_value(serde_json::json!("screen_on")).unwrap();
        assert_eq!(plain, EventAction::from(SessionAction::ScreenOn));
        assert!(!plain.while_remote());
        assert_eq!(
            serde_json::to_value(EventAction::from(SessionAction::ScreenOn)).unwrap(),
            serde_json::json!("screen_on")
        );

        // A table with `while_remote = true` round-trips to a
        // fire-while-armed action; the gate coexists with `restore_input`.
        let table = serde_json::json!({
            "action": "screen_on",
            "restore_input": true,
            "while_remote": true,
        });
        let ungated: EventAction = serde_json::from_value(table.clone()).unwrap();
        assert_eq!(
            ungated,
            EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_remote: true,
            }
        );
        assert!(ungated.restore_input());
        assert!(ungated.while_remote());
        assert_eq!(serde_json::to_value(ungated).unwrap(), table);

        // A table omitting `while_remote` defaults it to `false` (gated)
        // — absence gates, matching the bare token.
        let defaulted: EventAction =
            serde_json::from_value(serde_json::json!({ "action": "power_on" })).unwrap();
        assert_eq!(defaulted.action(), SessionAction::PowerOn);
        assert!(!defaulted.while_remote());
    }

    #[test]
    fn event_action_gate_toml_round_trips_in_a_profile() {
        // `while_remote = false` survives a full profile TOML round-trip
        // and coexists with `restore_input`; capture-arms and the
        // connect/disconnect actions ride the same body.
        let toml_str = r#"
name = "remote"
on_session_unlock = { action = "screen_on", while_remote = false }
on_idle_resume = { action = "screen_on", restore_input = true, while_remote = false }
on_remote_connect = "power_off"
on_remote_disconnect = "none"
remote_screencast_arms = true
"#;
        let body: UserProfileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            body.on_session_unlock,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: false,
                while_remote: false,
            })
        );
        assert_eq!(
            body.on_idle_resume,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_remote: false,
            })
        );
        assert_eq!(body.on_remote_connect, Some(SessionAction::PowerOff.into()));
        assert_eq!(body.on_remote_disconnect, Some(SessionAction::None.into()));
        assert_eq!(body.remote_screencast_arms, Some(true));
        // A bare-token field parses as `Plain` (gate implicitly false).
        assert!(body.on_lock_wake.is_none());

        // Re-serialize and re-parse: shapes are stable across the trip.
        let text = toml::to_string(&body).unwrap();
        let reparsed: UserProfileConfig = toml::from_str(&text).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn setting_an_event_replaces_the_whole_table() {
        // Whole-value merge (the `restore_input` precedent): gating one
        // event on a custom profile replaces that event's entire table,
        // so the base action must be restated alongside the gate. A
        // merge onto a base leaves the base's own action untouched only
        // because the custom value is the whole table.
        let base = UserProfileConfig {
            on_session_unlock: Some(EventAction::restoring(SessionAction::ScreenOn)),
            ..UserProfileConfig::default()
        };
        let custom = UserProfileConfig {
            on_session_unlock: Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_remote: false,
            }),
            ..UserProfileConfig::default()
        };
        let merged = custom.merge_with_base(&base);
        // The custom table wins wholesale — action + both modifiers.
        assert_eq!(
            merged.on_session_unlock,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: true,
                while_remote: false,
            })
        );
    }

    #[test]
    fn resolved_user_profile_defaults_blank_the_remote_edges() {
        let resolved = ResolvedUserProfile::default();
        // Both remote edges floor to screen_off: blank on arm, stay dark on
        // disarm. Capture arming stays off — detection is the opt-in.
        assert_eq!(resolved.on_remote_connect, SessionAction::ScreenOff);
        assert_eq!(resolved.on_remote_disconnect, SessionAction::ScreenOff);
        assert!(!resolved.remote_screencast_arms);
        // Floor splits by direction: the wake-ish user events are gated
        // (suppressed while armed)…
        assert!(!resolved.on_session_unlock.while_remote());
        assert!(!resolved.on_lock_wake.while_remote());
        assert!(!resolved.on_idle_resume.while_remote());
        // …while the off-going blanks fire while armed.
        assert!(resolved.on_session_lock.while_remote());
        assert!(resolved.on_idle_timeout.while_remote());
    }

    #[test]
    fn automate_when_display_fromstr_and_serde_agree() {
        for when in [
            AutomateWhen::Always,
            AutomateWhen::OnPrimary,
            AutomateWhen::OffPrimary,
        ] {
            let shown = when.to_string();
            assert_eq!(shown.parse::<AutomateWhen>(), Ok(when));
            assert_eq!(
                serde_json::to_value(when).unwrap(),
                serde_json::Value::String(shown)
            );
        }
        assert!("bogus".parse::<AutomateWhen>().is_err());
    }

    // =========================================================================
    // SessionAction tests
    // =========================================================================

    #[test]
    fn test_session_action_serialization() {
        assert_eq!(
            serde_json::to_string(&SessionAction::ScreenOff).unwrap(),
            "\"screen_off\""
        );
        assert_eq!(
            serde_json::from_str::<SessionAction>("\"power_on\"").unwrap(),
            SessionAction::PowerOn
        );
    }

    // =========================================================================
    // SystemProfileConfig tests
    // =========================================================================

    #[test]
    fn test_resolved_system_profile_default() {
        let resolved = ResolvedSystemProfile::default();
        assert_eq!(resolved.on_startup, SessionAction::PowerOn);
        assert_eq!(resolved.on_shutdown, SessionAction::PowerOff);
        assert_eq!(resolved.on_reboot, SessionAction::None);
        // DPMS actions mirror the host display by default.
        assert_eq!(resolved.on_dpms_off, SessionAction::ScreenOff);
        assert_eq!(resolved.on_dpms_on, SessionAction::ScreenOn);
        // Wake-ish stock events are gated while armed, so an unconfigured
        // profile keeps the panel dark for the empty room.
        assert!(!resolved.on_dpms_on.while_remote());
        assert!(!resolved.on_wake.while_remote());
        assert!(!resolved.on_startup.while_remote());
        assert!(!resolved.on_idle_resume.while_remote());
        // Off-going stock events fire while armed: the blanks (DPMS-off,
        // idle-timeout) alongside the power-offs (sleep/shutdown/reboot).
        assert!(resolved.on_dpms_off.while_remote());
        assert!(resolved.on_idle_timeout.while_remote());
        assert!(resolved.on_sleep.while_remote());
        assert!(resolved.on_shutdown.while_remote());
        assert!(resolved.on_reboot.while_remote());
    }

    #[test]
    fn system_profile_event_gate_round_trips() {
        // A system event's `while_remote = false` gate survives a TOML
        // round-trip and resolves through the chain.
        let toml_str = r#"
name = "gated"
on_dpms_on = { action = "screen_on", while_remote = false }
on_wake = { action = "power_on", restore_input = true, while_remote = false }
"#;
        let body: SystemProfileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            body.on_dpms_on,
            Some(EventAction::With {
                action: SessionAction::ScreenOn,
                restore_input: false,
                while_remote: false,
            })
        );
        assert!(!body.resolve().on_dpms_on.while_remote());
        assert!(!body.resolve().on_wake.while_remote());

        let text = toml::to_string(&body).unwrap();
        let reparsed: SystemProfileConfig = toml::from_str(&text).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn test_system_profile_full_default() {
        let full = SystemProfileConfig::full_default();
        let resolved = full.resolve();
        assert_eq!(resolved, ResolvedSystemProfile::default());
    }

    #[test]
    fn test_system_profile_merge() {
        let base = SystemProfileConfig {
            on_startup: Some(SessionAction::PowerOn.into()),
            on_shutdown: Some(SessionAction::PowerOff.into()),
            on_dpms_off: Some(SessionAction::ScreenOff.into()),
            ..SystemProfileConfig::default()
        };

        let override_profile = SystemProfileConfig {
            on_shutdown: Some(SessionAction::ScreenOff.into()),
            on_dpms_on: Some(SessionAction::ScreenOn.into()),
            ..SystemProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_startup, Some(SessionAction::PowerOn.into())); // from base
        assert_eq!(merged.on_shutdown, Some(SessionAction::ScreenOff.into())); // overridden
        assert_eq!(merged.on_dpms_off, Some(SessionAction::ScreenOff.into())); // from base
        assert_eq!(merged.on_dpms_on, Some(SessionAction::ScreenOn.into())); // overridden
    }

    // =========================================================================
    // ResolvedUserProfile tests
    // =========================================================================

    #[test]
    fn test_resolved_profile_default() {
        let resolved = ResolvedUserProfile::default();
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn);
        assert_eq!(resolved.on_lock_wake, SessionAction::ScreenOn);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 0);
        assert_eq!(resolved.on_idle_timeout, SessionAction::ScreenOff);
    }

    #[test]
    fn test_profile_config_full_default() {
        let full = UserProfileConfig::full_default();
        let resolved = full.resolve();
        assert_eq!(resolved, ResolvedUserProfile::default());
    }

    // =========================================================================
    // Profile merge/resolve tests
    // =========================================================================

    #[test]
    fn test_merge_with_base_partial_override() {
        let base = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let override_profile = UserProfileConfig {
            on_session_lock: Some(SessionAction::None.into()),
            // unlocked_idle_timeout_seconds not specified — should inherit from base
            ..UserProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_session_lock, Some(SessionAction::None.into()));
        assert_eq!(merged.unlocked_idle_timeout_seconds, Some(300)); // inherited
    }

    #[test]
    fn test_merge_with_base_empty_override() {
        let base = UserProfileConfig::full_default();
        let empty = UserProfileConfig::default(); // all None

        let merged = empty.merge_with_base(&base);
        // All fields should come from base
        assert_eq!(merged.on_session_lock, base.on_session_lock);
        assert_eq!(
            merged.unlocked_idle_timeout_seconds,
            base.unlocked_idle_timeout_seconds
        );
    }

    #[test]
    fn test_merge_with_base_full_override() {
        let base = UserProfileConfig::full_default();
        let full_override = UserProfileConfig {
            name: String::new(),
            on_session_lock: Some(SessionAction::None.into()),
            on_session_unlock: Some(SessionAction::None.into()),
            on_lock_wake: Some(SessionAction::None.into()),
            locked_idle_timeout_seconds: Some(30),
            unlocked_idle_timeout_seconds: Some(999),
            on_idle_timeout: Some(SessionAction::PowerOff.into()),
            on_idle_resume: Some(SessionAction::None.into()),
            ..Default::default()
        };

        let merged = full_override.merge_with_base(&base);
        assert_eq!(merged.on_session_lock, Some(SessionAction::None.into()));
        assert_eq!(merged.locked_idle_timeout_seconds, Some(30));
        assert_eq!(merged.unlocked_idle_timeout_seconds, Some(999));
        assert_eq!(merged.on_idle_timeout, Some(SessionAction::PowerOff.into()));
    }

    #[test]
    fn test_resolve_fills_nones() {
        let partial = UserProfileConfig {
            on_session_lock: Some(SessionAction::None.into()),
            // Everything else is None
            ..UserProfileConfig::default()
        };

        let resolved = partial.resolve();
        assert_eq!(resolved.on_session_lock, SessionAction::None); // from partial
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn); // hard-coded default
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 0); // hard-coded default
    }

    // =========================================================================
    // UserConfig tests
    // =========================================================================

    #[test]
    fn test_parse_user_config() {
        let gaming_uuid = "a1b2c3d4-0000-0000-0000-000000000020";
        let toml_str = format!(
            r#"
[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
unlocked_idle_timeout_seconds = 600

[user.profiles."{gaming_uuid}"]
name = "gaming"
on_session_lock = "none"
unlocked_idle_timeout_seconds = 0

[[tv]]
uuid = "11111111-1111-1111-1111-111111111111"
profile.user = "{gaming_uuid}"
"#
        );

        let config: UserConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].uuid, "11111111-1111-1111-1111-111111111111");
        assert_eq!(
            config.tv[0].profile.user,
            gaming_uuid.parse::<Uuid>().unwrap()
        );
    }

    #[test]
    fn test_user_config_resolve_user_profile_with_assignment() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None.into()),
                // unlocked_idle_timeout_seconds not specified — should inherit from system
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living_room", gaming_uuid);

        let resolved = user.resolve_user_profile("living_room", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::None); // user gaming
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 300); // inherited from system
    }

    #[test]
    fn test_user_config_resolve_user_profile_no_assignment() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let user = UserConfig::default(); // no profiles, no TV assignments

        // TV without assignment uses system default
        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 300);
    }

    #[test]
    fn test_user_config_resolve_user_profile_user_default_overrides_system() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                unlocked_idle_timeout_seconds: Some(600), // user overrides system
                ..UserProfileConfig::default()
            },
        );

        // TV without explicit assignment uses user's default profile
        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff); // from system
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 600); // user override
    }

    #[test]
    fn test_user_config_resolve_user_profile_named_inherits_user_default() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(100),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        // User's default profile overrides system idle timeout
        user.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                unlocked_idle_timeout_seconds: Some(600),
                ..UserProfileConfig::default()
            },
        );
        // User's gaming profile overrides screen lock only
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None.into()),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living_room", gaming_uuid);

        let resolved = user.resolve_user_profile("living_room", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::None); // gaming
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 600); // user default (not system 100)
    }

    #[test]
    fn test_user_config_get_tv_user_profile() {
        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user.set_tv_user_profile("living_room", gaming_uuid);

        assert_eq!(user.get_tv_user_profile("living_room"), gaming_uuid);
        // Unassigned TV defaults to the reserved default UUID.
        assert_eq!(
            user.get_tv_user_profile("office"),
            DEFAULT_USER_PROFILE_UUID
        );
    }

    #[test]
    fn test_user_config_roundtrip() {
        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None.into()),
                unlocked_idle_timeout_seconds: Some(0),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living_room", gaming_uuid);

        let serialized = toml::to_string_pretty(&user).unwrap();
        let deserialized: UserConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.user.profiles.len(), 1);
        let gaming = &deserialized.user.profiles[&gaming_uuid];
        assert_eq!(gaming.on_session_lock, Some(SessionAction::None.into()));
        assert_eq!(gaming.unlocked_idle_timeout_seconds, Some(0));
        // None fields should not be serialized
        let living = deserialized
            .tv
            .iter()
            .find(|t| t.uuid == "living_room")
            .unwrap();
        assert_eq!(living.profile.user, gaming_uuid);
    }

    #[test]
    fn test_parse_profile_config_all_fields() {
        let sys_profile: SystemProfileConfig = toml::from_str(
            r#"
name = "custom"
on_sleep = "none"
on_wake = "screen_on"
on_dpms_off = "screen_off"
on_dpms_on = "screen_on"
"#,
        )
        .unwrap();
        assert_eq!(sys_profile.on_sleep, Some(SessionAction::None.into()));
        assert_eq!(sys_profile.on_wake, Some(SessionAction::ScreenOn.into()));
        assert_eq!(
            sys_profile.on_dpms_off,
            Some(SessionAction::ScreenOff.into())
        );
        assert_eq!(sys_profile.on_dpms_on, Some(SessionAction::ScreenOn.into()));

        let profile: UserProfileConfig = toml::from_str(
            r#"
name = "custom"
on_session_lock = "power_off"
on_session_unlock = "power_on"
on_lock_wake = "none"
unlocked_idle_timeout_seconds = 300
on_idle_timeout = "screen_off"
"#,
        )
        .unwrap();
        assert_eq!(
            profile.on_session_lock,
            Some(SessionAction::PowerOff.into())
        );
        assert_eq!(
            profile.on_session_unlock,
            Some(SessionAction::PowerOn.into())
        );
        assert_eq!(profile.on_lock_wake, Some(SessionAction::None.into()));
        assert_eq!(profile.unlocked_idle_timeout_seconds, Some(300));
        assert_eq!(
            profile.on_idle_timeout,
            Some(SessionAction::ScreenOff.into())
        );

        // Resolve to concrete values
        let resolved = profile.resolve();
        assert_eq!(resolved.on_session_lock, SessionAction::PowerOff);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 300);
    }

    // =========================================================================
    // System + User profile merging (simulates session service behavior)
    // =========================================================================

    #[test]
    fn test_merged_profile_list_system_only() {
        // When user has no config, all profiles come from system config
        let system_profiles: HashMap<String, UserProfileConfig> = [
            (
                "default".to_string(),
                UserProfileConfig {
                    on_session_lock: Some(SessionAction::ScreenOff.into()),
                    ..UserProfileConfig::default()
                },
            ),
            (
                "foo".to_string(),
                UserProfileConfig {
                    on_session_lock: Some(SessionAction::None.into()),
                    ..UserProfileConfig::default()
                },
            ),
        ]
        .into_iter()
        .collect();

        let user_config = UserConfig::default(); // empty

        // Merge: system profiles as base, user profiles override.
        // Keyed by profile name — same shape the session service's list-merge uses.
        let mut merged: HashMap<String, &UserProfileConfig> = HashMap::new();
        for (name, profile) in &system_profiles {
            merged.insert(name.clone(), profile);
        }
        for profile in user_config.user.profiles.values() {
            merged.insert(profile.name.clone(), profile);
        }

        assert_eq!(merged.len(), 2);
        assert!(merged.contains_key("default"));
        assert!(merged.contains_key("foo"));
    }

    #[test]
    fn test_merged_profile_list_user_overrides_system() {
        // User defines "default" and "gaming", system has "default" and "foo"
        let system_profiles: HashMap<String, UserProfileConfig> = [
            (
                "default".to_string(),
                UserProfileConfig {
                    name: "default".to_string(),
                    on_session_lock: Some(SessionAction::ScreenOff.into()),
                    unlocked_idle_timeout_seconds: Some(300),
                    ..UserProfileConfig::default()
                },
            ),
            (
                "foo".to_string(),
                UserProfileConfig {
                    name: "foo".to_string(),
                    on_session_lock: Some(SessionAction::None.into()),
                    ..UserProfileConfig::default()
                },
            ),
        ]
        .into_iter()
        .collect();

        let mut user_config = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user_config.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                unlocked_idle_timeout_seconds: Some(600), // user overrides
                ..UserProfileConfig::default()
            },
        );
        user_config.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None.into()),
                ..UserProfileConfig::default()
            },
        );

        // Merge: system first, then user overrides. Profile name is the
        // key (matches the session service's user-profile list merge).
        let mut merged: HashMap<String, &UserProfileConfig> = HashMap::new();
        for (name, profile) in &system_profiles {
            merged.insert(name.clone(), profile);
        }
        for profile in user_config.user.profiles.values() {
            merged.insert(profile.name.clone(), profile);
        }

        // Should have 3 profiles: default (user), foo (system), gaming (user)
        assert_eq!(merged.len(), 3);
        assert!(merged.contains_key("default"));
        assert!(merged.contains_key("foo"));
        assert!(merged.contains_key("gaming"));

        // "default" should be the user's version (unlocked_idle_timeout_seconds = 600)
        let default = &merged["default"];
        assert_eq!(default.unlocked_idle_timeout_seconds, Some(600));

        // "foo" comes from system
        let foo = &merged["foo"];
        assert_eq!(foo.on_session_lock, Some(SessionAction::None.into()));
    }

    #[test]
    fn test_user_config_ignores_unknown_toml_fields() {
        // UserConfig should parse even if the TOML has extra fields
        let gaming_uuid = "a1b2c3d4-0000-0000-0000-000000000020";
        let user_only_toml = format!(
            r#"
[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
unlocked_idle_timeout_seconds = 600

[user.profiles."{gaming_uuid}"]
name = "gaming"
on_session_lock = "none"

[[tv]]
uuid = "11111111-1111-1111-1111-111111111111"
profile.user = "{gaming_uuid}"
"#
        );

        let config: UserConfig = toml::from_str(&user_only_toml).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert!(config.user.find_profile_by_name("default").is_some());
        assert!(config.user.find_profile_by_name("gaming").is_some());
        let living = config
            .tv
            .iter()
            .find(|t| t.uuid == "11111111-1111-1111-1111-111111111111")
            .unwrap();
        assert_eq!(living.profile.user, gaming_uuid.parse::<Uuid>().unwrap());
    }

    #[test]
    fn test_system_config_parses_as_user_config() {
        // A system config with [[tv]] (array of tables) can now be parsed as UserConfig
        // because UserConfig.tv is also a Vec. UserTvConfig ignores unknown fields
        // like name, vendor, lg, etc.
        let foo_uuid = "a1b2c3d4-0000-0000-0000-000000000040";
        let system_toml = format!(
            r#"
[system.profiles."00000000-0000-0000-0000-000000005159"]
name = "default"
on_startup = "power_on"

[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
on_session_lock = "screen_off"

[user.profiles."{foo_uuid}"]
name = "foo"
on_session_lock = "none"

[[tv]]
uuid = "22222222-2222-2222-2222-222222222222"
name = "Office"
vendor = "lg"
managed = true

[tv.lg]
ip = "192.168.1.1"
"#
        );

        let config: UserConfig = toml::from_str(&system_toml).unwrap();
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].uuid, "22222222-2222-2222-2222-222222222222");
        // profile.user defaults to DEFAULT_USER_PROFILE_UUID since it's not specified
        assert_eq!(config.tv[0].profile.user, DEFAULT_USER_PROFILE_UUID);
    }

    /// Test that on_idle_resume is configurable and defaults to ScreenOn.
    #[test]
    fn test_on_idle_resume_configurable() {
        // Default resolves to ScreenOn
        let default_profile = ResolvedUserProfile::default();
        assert_eq!(default_profile.on_idle_resume, SessionAction::ScreenOn);

        // Can be overridden to None
        let profile = UserProfileConfig {
            on_idle_resume: Some(SessionAction::None.into()),
            ..UserProfileConfig::default()
        };
        let resolved = profile.resolve();
        assert_eq!(resolved.on_idle_resume, SessionAction::None);

        // Inherits from system default if not set in user profile
        let system = UserProfileConfig {
            on_idle_resume: Some(SessionAction::PowerOn.into()),
            ..UserProfileConfig::default()
        };
        let user_profile = UserProfileConfig::default(); // on_idle_resume = None
        let merged = user_profile.merge_with_base(&system).resolve();
        assert_eq!(merged.on_idle_resume, SessionAction::PowerOn);
    }

    /// Regression test: when a TV's assigned profile UUID does not exist
    /// in the user config, resolution must fall through to the user's
    /// default profile body before reaching the system default.
    #[test]
    fn test_user_config_resolve_user_profile_missing_profile_falls_through_user_default() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                unlocked_idle_timeout_seconds: Some(600),
                ..UserProfileConfig::default()
            },
        );
        // Stale UUID with no body — resolution falls through to user default.
        user.set_tv_user_profile("living_room", Uuid::new_v4());

        let resolved = user.resolve_user_profile("living_room", &system_default);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 600);
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
    }

    #[test]
    fn test_resolve_user_profile_with_system_profiles_as_base() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff.into()),
            on_session_unlock: Some(SessionAction::ScreenOn.into()),
            unlocked_idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None.into()),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("office", gaming_uuid);

        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::None);
        assert_eq!(resolved.unlocked_idle_timeout_seconds, 300);
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn);

        let resolved_unknown = user.resolve_user_profile("other_tv", &system_default);
        assert_eq!(resolved_unknown.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved_unknown.unlocked_idle_timeout_seconds, 300);
    }
}
