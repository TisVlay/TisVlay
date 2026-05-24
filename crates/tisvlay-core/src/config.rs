//! Configuration file parsing and management.
//!
//! # Configuration Split
//!
//! The configuration is split between system and user levels:
//!
//! ## System Config (`/var/lib/tisvlay/config.toml`)
//!
//! Owned by `tisvlayd`. Contains TV definitions, credentials, system profiles
//! for daemon lifecycle events, and user profile defaults inherited by users.
//!
//! ```toml
//! [system.profiles."00000000-0000-0000-0000-000000005159"]
//! name = "default"
//! on_startup = "power_on"
//! on_shutdown = "power_off"
//! on_reboot = "none"
//! on_no_session = "none"
//! on_sleep = "power_off"
//! on_wake = "power_on"
//!
//! [user.profiles."00000000-0000-0000-0000-00000000515a"]
//! name = "default"
//! on_session_lock = "screen_off"
//! on_session_unlock = "screen_on"
//!
//! [[tv]]
//! uuid = "11111111-1111-1111-1111-111111111111"
//! name = "Living Room TV"
//! vendor = "lg"
//! primary_input = "HDMI_1"
//! profile.system = "00000000-0000-0000-0000-000000005159"
//!
//! [tv.lg]
//! ip = "192.168.1.100"
//! mac = "AA:BB:CC:DD:EE:FF"
//! client_key = "abc123"
//! ```
//!
//! ## User Config (`~/.config/tisvlay/session.toml`)
//!
//! Owned by each user's `tisvlay-session`. Contains profile definitions
//! and per-TV profile assignments. Optional — if absent, the session service
//! inherits the system default profile.
//!
//! ```toml
//! [user.profiles."00000000-0000-0000-0000-00000000515a"]
//! name = "default"
//! idle_timeout_seconds = 600
//!
//! [user.profiles."a1b2c3d4-0000-0000-0000-000000000020"]
//! name = "gaming"
//! on_session_lock = "none"
//! idle_timeout_seconds = 0
//!
//! [[tv]]
//! uuid = "11111111-1111-1111-1111-111111111111"
//! profile.user = "a1b2c3d4-0000-0000-0000-000000000020"
//! ```

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// =============================================================================
// Actions
// =============================================================================

/// Actions the session service or daemon can take in response to events.
//
// SAFETY: the `signature = "s"` attribute is a hand-declared wire-format
// promise that this enum stays unit-variant-only. Adding a struct variant
// would silently diverge the declared signature from what serde emits
// (string per-variant via `rename_all = "snake_case"`), with no compile
// error — so the type stays append-only-of-unit-variants. Same goes for
// `AutomateWhen` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
#[cfg_attr(feature = "dbus", zvariant(signature = "s"))]
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

impl std::fmt::Display for SessionAction {
    /// Stable snake_case identifier — matches the wire form
    /// `Session1.AutomationFired` emits as its `action_kind` field
    /// and the keys clients use for `gettext` lookup
    /// (`automation.action.<kind>`). Identical to the
    /// `serde(rename_all = "snake_case")` form so a fresh
    /// `SessionAction::deserialize` of any of these strings
    /// round-trips cleanly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::None => "none",
            Self::ScreenOff => "screen_off",
            Self::ScreenOn => "screen_on",
            Self::PowerOff => "power_off",
            Self::PowerOn => "power_on",
        };
        f.write_str(s)
    }
}

// =============================================================================
// AutomateWhen — input-aware automation gating
// =============================================================================

/// Controls when automation fires relative to the TV's current input.
///
/// Combined with the per-TV `primary_input` and the global `managed` flag
/// to produce the `Automated` D-Bus property.
///
/// See the SAFETY note on `SessionAction` — the `signature = "s"` attribute
/// keeps this enum unit-variant-only as a wire-format invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "dbus", derive(zbus::zvariant::Type))]
#[cfg_attr(feature = "dbus", zvariant(signature = "s"))]
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
    pub on_startup: Option<SessionAction>,

    /// Action when the system powers off or halts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_shutdown: Option<SessionAction>,

    /// Action when the system reboots (including kexec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_reboot: Option<SessionAction>,

    /// Action when all user sessions end (e.g., after logout, login screen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_no_session: Option<SessionAction>,

    /// Action on system sleep/suspend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_sleep: Option<SessionAction>,

    /// Action on system wake/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wake: Option<SessionAction>,

    /// When to apply automation relative to the TV's current input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automate_when: Option<AutomateWhen>,
}

impl SystemProfileConfig {
    /// Create a profile with all fields set to the built-in sensible defaults.
    ///
    /// Use this when saving a system config to make profiles self-documenting.
    /// The `name` field is left empty — callers set it when materializing
    /// the reserved default or naming a new profile.
    pub fn full_default() -> Self {
        let d = ResolvedSystemProfile::default();
        Self {
            name: String::new(),
            on_startup: Some(d.on_startup),
            on_shutdown: Some(d.on_shutdown),
            on_reboot: Some(d.on_reboot),
            on_no_session: Some(d.on_no_session),
            on_sleep: Some(d.on_sleep),
            on_wake: Some(d.on_wake),
            automate_when: Some(d.automate_when),
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`.
    /// Fields that are `Some` in `self` take precedence. The `name`
    /// field is preserved from `self` — names are identity-adjacent
    /// metadata, not inheritable behavior.
    pub fn merge_with_base(&self, base: &SystemProfileConfig) -> SystemProfileConfig {
        SystemProfileConfig {
            name: self.name.clone(),
            on_startup: self.on_startup.or(base.on_startup),
            on_shutdown: self.on_shutdown.or(base.on_shutdown),
            on_reboot: self.on_reboot.or(base.on_reboot),
            on_no_session: self.on_no_session.or(base.on_no_session),
            on_sleep: self.on_sleep.or(base.on_sleep),
            on_wake: self.on_wake.or(base.on_wake),
            automate_when: self.automate_when.or(base.automate_when),
        }
    }

    /// Resolve this profile to concrete values.
    ///
    /// Any `None` fields are filled with hard-coded defaults from
    /// [`ResolvedSystemProfile::default()`].
    pub fn resolve(&self) -> ResolvedSystemProfile {
        let d = ResolvedSystemProfile::default();
        ResolvedSystemProfile {
            on_startup: self.on_startup.unwrap_or(d.on_startup),
            on_shutdown: self.on_shutdown.unwrap_or(d.on_shutdown),
            on_reboot: self.on_reboot.unwrap_or(d.on_reboot),
            on_no_session: self.on_no_session.unwrap_or(d.on_no_session),
            on_sleep: self.on_sleep.unwrap_or(d.on_sleep),
            on_wake: self.on_wake.unwrap_or(d.on_wake),
            automate_when: self.automate_when.unwrap_or(d.automate_when),
        }
    }
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
    pub on_startup: SessionAction,
    /// Action when the system powers off or halts.
    pub on_shutdown: SessionAction,
    /// Action when the system reboots (including kexec).
    pub on_reboot: SessionAction,
    /// Action when all user sessions end.
    pub on_no_session: SessionAction,
    /// Action on system sleep/suspend.
    pub on_sleep: SessionAction,
    /// Action on system wake/resume.
    pub on_wake: SessionAction,
    /// When to apply automation relative to the TV's current input.
    pub automate_when: AutomateWhen,
}

impl Default for ResolvedSystemProfile {
    fn default() -> Self {
        Self {
            on_startup: SessionAction::PowerOn,
            on_shutdown: SessionAction::PowerOff,
            on_reboot: SessionAction::None,
            on_no_session: SessionAction::None,
            on_sleep: SessionAction::PowerOff,
            on_wake: SessionAction::PowerOn,
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
    pub on_session_lock: Option<SessionAction>,

    /// Action on session unlock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_session_unlock: Option<SessionAction>,

    /// Action when user interacts with the lock screen greeter (before unlocking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_greeter_wake: Option<SessionAction>,

    /// Idle timeout in seconds (0 to disable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<u64>,

    /// Action on idle timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_timeout: Option<SessionAction>,

    /// Action when activity resumes after idle timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_resume: Option<SessionAction>,
}

impl UserProfileConfig {
    /// Create a profile with all fields set to the built-in sensible defaults.
    ///
    /// Use this when saving a system config to make profiles self-documenting,
    /// or as the system default profile. The `name` field is left empty —
    /// callers set it when materializing the reserved default or naming
    /// a new profile.
    pub fn full_default() -> Self {
        let d = ResolvedUserProfile::default();
        Self {
            name: String::new(),
            on_session_lock: Some(d.on_session_lock),
            on_session_unlock: Some(d.on_session_unlock),
            on_greeter_wake: Some(d.on_greeter_wake),
            idle_timeout_seconds: Some(d.idle_timeout_seconds),
            on_idle_timeout: Some(d.on_idle_timeout),
            on_idle_resume: Some(d.on_idle_resume),
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`.
    /// Fields that are `Some` in `self` take precedence. The `name`
    /// field is preserved from `self`.
    pub fn merge_with_base(&self, base: &UserProfileConfig) -> UserProfileConfig {
        UserProfileConfig {
            name: self.name.clone(),
            on_session_lock: self.on_session_lock.or(base.on_session_lock),
            on_session_unlock: self.on_session_unlock.or(base.on_session_unlock),
            on_greeter_wake: self.on_greeter_wake.or(base.on_greeter_wake),
            idle_timeout_seconds: self.idle_timeout_seconds.or(base.idle_timeout_seconds),
            on_idle_timeout: self.on_idle_timeout.or(base.on_idle_timeout),
            on_idle_resume: self.on_idle_resume.or(base.on_idle_resume),
        }
    }

    /// Resolve this profile to concrete values.
    ///
    /// Any `None` fields are filled with hard-coded defaults from
    /// [`ResolvedUserProfile::default()`].
    pub fn resolve(&self) -> ResolvedUserProfile {
        let d = ResolvedUserProfile::default();
        ResolvedUserProfile {
            on_session_lock: self.on_session_lock.unwrap_or(d.on_session_lock),
            on_session_unlock: self.on_session_unlock.unwrap_or(d.on_session_unlock),
            on_greeter_wake: self.on_greeter_wake.unwrap_or(d.on_greeter_wake),
            idle_timeout_seconds: self.idle_timeout_seconds.unwrap_or(d.idle_timeout_seconds),
            on_idle_timeout: self.on_idle_timeout.unwrap_or(d.on_idle_timeout),
            on_idle_resume: self.on_idle_resume.unwrap_or(d.on_idle_resume),
        }
    }
}

// =============================================================================
// ResolvedUserProfile (fully resolved, no Option fields)
// =============================================================================

/// A fully resolved profile with no optional fields.
///
/// This is the result of merging a [`UserProfileConfig`] chain
/// (user profile -> system default -> hard-coded defaults).
/// Consumers (session monitor, idle detector) use this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUserProfile {
    /// Action on session lock event.
    pub on_session_lock: SessionAction,
    /// Action on session unlock event.
    pub on_session_unlock: SessionAction,
    /// Action when user interacts with the lock screen greeter.
    pub on_greeter_wake: SessionAction,
    /// Idle timeout in seconds (0 to disable).
    pub idle_timeout_seconds: u64,
    /// Action on idle timeout.
    pub on_idle_timeout: SessionAction,
    /// Action when activity resumes after idle timeout.
    pub on_idle_resume: SessionAction,
}

impl Default for ResolvedUserProfile {
    fn default() -> Self {
        Self {
            on_session_lock: SessionAction::ScreenOff,
            on_session_unlock: SessionAction::ScreenOn,
            on_greeter_wake: SessionAction::ScreenOn,
            idle_timeout_seconds: 0,
            on_idle_timeout: SessionAction::ScreenOff,
            on_idle_resume: SessionAction::ScreenOn,
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
// SystemProfileBody — wire body for the D-Bus property-bag CRUD surface
// =============================================================================

/// Wire-format body for a system profile, exchanged across `Manager1`'s
/// CRUD methods as an `a{sv}` dict.
///
/// `name` is the mutable display label (always present on the wire);
/// every other field is `Option<T>` so an absent key signals
/// "inherits from the chain below" — exactly the present-vs-absent
/// semantic clients need without a separate `LocallySetFields`
/// companion property. `deny_unknown_fields` rejects mis-spelled keys
/// at the dict-derive boundary instead of silently absorbing them
/// as `None`.
///
/// Lossless round-trip with [`SystemProfileConfig`] via the `From`
/// impls below — the on-disk shape (TOML) keeps the same
/// inheritable-`Option<T>` layout; only `name` lives here as a body
/// field rather than the TOML section key.
#[cfg(feature = "dbus")]
#[derive(
    Debug,
    Clone,
    Default,
    zbus::zvariant::Type,
    zbus::zvariant::SerializeDict,
    zbus::zvariant::DeserializeDict,
)]
#[zvariant(signature = "a{sv}", deny_unknown_fields)]
pub struct SystemProfileBody {
    /// Mutable display label. Always present on the wire (the dict-derive
    /// emits it unconditionally). Renames are body updates with a new
    /// `name`; UUID identity stays stable.
    pub name: String,
    pub on_startup: Option<SessionAction>,
    pub on_shutdown: Option<SessionAction>,
    pub on_reboot: Option<SessionAction>,
    pub on_no_session: Option<SessionAction>,
    pub on_sleep: Option<SessionAction>,
    pub on_wake: Option<SessionAction>,
    pub automate_when: Option<AutomateWhen>,
}

#[cfg(feature = "dbus")]
impl SystemProfileBody {
    /// Construct a body from a `SystemProfileConfig`. The config now
    /// carries the display name itself, so the body just clones it
    /// across — no separate name argument.
    pub fn from_config(config: &SystemProfileConfig) -> Self {
        Self {
            name: config.name.clone(),
            on_startup: config.on_startup,
            on_shutdown: config.on_shutdown,
            on_reboot: config.on_reboot,
            on_no_session: config.on_no_session,
            on_sleep: config.on_sleep,
            on_wake: config.on_wake,
            automate_when: config.automate_when,
        }
    }
}

#[cfg(feature = "dbus")]
impl From<&SystemProfileBody> for SystemProfileConfig {
    /// Round-trip: the body's `name` lands on the config's `name` so
    /// the on-disk shape preserves the display label.
    fn from(body: &SystemProfileBody) -> Self {
        SystemProfileConfig {
            name: body.name.clone(),
            on_startup: body.on_startup,
            on_shutdown: body.on_shutdown,
            on_reboot: body.on_reboot,
            on_no_session: body.on_no_session,
            on_sleep: body.on_sleep,
            on_wake: body.on_wake,
            automate_when: body.automate_when,
        }
    }
}

// =============================================================================
// UserProfileBody — mirror of SystemProfileBody on the session bus
// =============================================================================

/// Wire-format body for a user profile. Shape mirrors [`SystemProfileBody`]
/// — `name` always present, every other field `Option<T>` for the
/// inherit-from-chain semantic. `idle_timeout_seconds` is the only numeric
/// field on either body; the rest are enum-typed `SessionAction`s.
#[cfg(feature = "dbus")]
#[derive(
    Debug,
    Clone,
    Default,
    zbus::zvariant::Type,
    zbus::zvariant::SerializeDict,
    zbus::zvariant::DeserializeDict,
)]
#[zvariant(signature = "a{sv}", deny_unknown_fields)]
pub struct UserProfileBody {
    pub name: String,
    pub on_session_lock: Option<SessionAction>,
    pub on_session_unlock: Option<SessionAction>,
    pub on_greeter_wake: Option<SessionAction>,
    pub on_idle_timeout: Option<SessionAction>,
    pub on_idle_resume: Option<SessionAction>,
    pub idle_timeout_seconds: Option<u64>,
}

#[cfg(feature = "dbus")]
impl UserProfileBody {
    /// Construct a body from a `UserProfileConfig`. The config now
    /// carries the display name itself.
    pub fn from_config(config: &UserProfileConfig) -> Self {
        Self {
            name: config.name.clone(),
            on_session_lock: config.on_session_lock,
            on_session_unlock: config.on_session_unlock,
            on_greeter_wake: config.on_greeter_wake,
            on_idle_timeout: config.on_idle_timeout,
            on_idle_resume: config.on_idle_resume,
            idle_timeout_seconds: config.idle_timeout_seconds,
        }
    }
}

#[cfg(feature = "dbus")]
impl From<&UserProfileBody> for UserProfileConfig {
    fn from(body: &UserProfileBody) -> Self {
        UserProfileConfig {
            name: body.name.clone(),
            on_session_lock: body.on_session_lock,
            on_session_unlock: body.on_session_unlock,
            on_greeter_wake: body.on_greeter_wake,
            on_idle_timeout: body.on_idle_timeout,
            on_idle_resume: body.on_idle_resume,
            idle_timeout_seconds: body.idle_timeout_seconds,
        }
    }
}

// =============================================================================
// NotificationSettingsBody — wire body for `Tray1.NotificationSettings`
// =============================================================================

/// Wire-format body for the tray's desktop-notification settings.
///
/// Each field is `Option<bool>` so that `Tray1.UpdateNotificationSettings`
/// can be a partial update: only the keys the caller wants to change
/// need to be set. The property getter (`Tray1.NotificationSettings`)
/// returns every field as `Some` (an absent field on the read side
/// would mean "this version of the tray doesn't know about this
/// category" — forward-compat across version skew).
///
/// The on-disk twin (`tisvlay_tray::config::NotificationSettings`) lives
/// in the tray crate; this body type is core's wire vocabulary so the
/// GUI client and the tray server both speak it. Per the workspace
/// convention in `docs/design.md`, wire bodies live in core; on-disk
/// containers stay with their owner.
#[cfg(feature = "dbus")]
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    zbus::zvariant::Type,
    zbus::zvariant::SerializeDict,
    zbus::zvariant::DeserializeDict,
    zbus::zvariant::Value,
    zbus::zvariant::OwnedValue,
)]
#[zvariant(signature = "a{sv}", deny_unknown_fields)]
pub struct NotificationSettingsBody {
    pub tv_connection: Option<bool>,
    pub tv_pairing: Option<bool>,
    pub tv_wake: Option<bool>,
    pub automation_fired: Option<bool>,
    pub config_reloaded: Option<bool>,
    pub daemon_offline: Option<bool>,
}

#[cfg(feature = "dbus")]
impl NotificationSettingsBody {
    /// Factory defaults — every category populated with its documented
    /// default-on value. Single source of truth shared by:
    /// - the tray, which uses this to seed a fresh `TrayConfig` when
    ///   `tray.toml` doesn't exist yet;
    /// - the GUI, which uses this to seed the Slint `Tray.notifications`
    ///   global before any successful fetch lands (so a GUI launched
    ///   while the tray is offline still shows sensible defaults).
    ///
    /// Distinct from the derived `Default` impl (all-`None`), which
    /// represents the partial-update neutral for
    /// `Tray1.UpdateNotificationSettings`.
    pub fn defaults() -> Self {
        Self {
            tv_connection: Some(true),
            tv_pairing: Some(true),
            tv_wake: Some(true),
            automation_fired: Some(false),
            config_reloaded: Some(false),
            daemon_offline: Some(true),
        }
    }
}

// =============================================================================
// Config sections
// =============================================================================

/// System-level configuration section.
///
/// Contains system profiles that define daemon lifecycle behavior.
/// Located under `[system]` in the TOML config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemSection {
    /// System profiles keyed by stable UUID. The reserved
    /// [`DEFAULT_SYSTEM_PROFILE_UUID`] entry is the inheritance baseline
    /// — daemon ensures it exists at load time via
    /// [`SystemSection::ensure_default`].
    ///
    /// `[[tv]]` entries reference profiles by UUID (`profile.system
    /// = "<uuid>"`). Identity is fully UUID-keyed end-to-end so
    /// renames don't touch any reference site.
    #[serde(default)]
    pub profiles: BTreeMap<Uuid, SystemProfileConfig>,
}

impl SystemSection {
    /// Resolve a system profile by UUID.
    ///
    /// Resolution chain: this profile body → [`DEFAULT_SYSTEM_PROFILE_UUID`]
    /// profile body → hard-coded defaults. UUIDs that don't match
    /// any profile fall through to the default-or-baseline path
    /// silently — a stale `profile.system = "<uuid>"` on a TV
    /// mustn't break dispatch after the profile is deleted.
    pub fn resolve_profile_by_uuid(&self, uuid: Uuid) -> ResolvedSystemProfile {
        let named = self.profiles.get(&uuid);
        let default = if uuid != DEFAULT_SYSTEM_PROFILE_UUID {
            self.profiles.get(&DEFAULT_SYSTEM_PROFILE_UUID)
        } else {
            None
        };

        match (named, default) {
            (Some(p), Some(d)) => p.merge_with_base(d).resolve(),
            (Some(p), None) => p.resolve(),
            (None, Some(d)) => d.resolve(),
            (None, None) => ResolvedSystemProfile::default(),
        }
    }

    /// Find a profile entry by its display name. Returns the UUID + body
    /// of the first match — names are enforced unique elsewhere, so
    /// "first" == "only" in practice.
    pub fn find_profile_by_name(&self, name: &str) -> Option<(Uuid, &SystemProfileConfig)> {
        self.profiles
            .iter()
            .find(|(_, body)| body.name == name)
            .map(|(uuid, body)| (*uuid, body))
    }

    /// Materialize the reserved-default profile if missing, then force
    /// its `name` field to [`DEFAULT_PROFILE_NAME`]. Idempotent.
    ///
    /// Called from `SystemConfig::load_from` (and the harness) so the
    /// catalog always has the reserved-default profile available for
    /// inheritance resolution and for `Tv1.SystemProfile` validation.
    pub fn ensure_default(&mut self) {
        self.profiles
            .entry(DEFAULT_SYSTEM_PROFILE_UUID)
            .or_insert_with(SystemProfileConfig::full_default);
        // Force the reserved name regardless of what was on disk —
        // the friendly name is just a label, but pinning it keeps
        // any name-based lookup deterministic for the default.
        if let Some(body) = self.profiles.get_mut(&DEFAULT_SYSTEM_PROFILE_UUID) {
            body.name = DEFAULT_PROFILE_NAME.to_string();
        }
    }
}

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
// TvProfileAssignment
// =============================================================================

/// Per-TV profile assignment.
///
/// Stored on `TvConfig` (in the system config's `[[tv]]` entries).
/// `system` is the UUID of the system profile governing daemon
/// lifecycle behavior; defaults to [`DEFAULT_SYSTEM_PROFILE_UUID`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvProfileAssignment {
    /// UUID of the system profile to use. Defaults to
    /// [`DEFAULT_SYSTEM_PROFILE_UUID`].
    #[serde(default = "default_system_profile_uuid")]
    pub system: Uuid,
}

impl Default for TvProfileAssignment {
    fn default() -> Self {
        Self {
            system: DEFAULT_SYSTEM_PROFILE_UUID,
        }
    }
}

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

fn default_system_profile_uuid() -> Uuid {
    DEFAULT_SYSTEM_PROFILE_UUID
}

fn default_user_profile_uuid() -> Uuid {
    DEFAULT_USER_PROFILE_UUID
}

// =============================================================================
// TvConfig
// =============================================================================

/// Configuration for a single TV.
///
/// This struct contains vendor-agnostic identity and hardware settings.
/// Connection parameters (IP, MAC, etc.) are vendor-specific and stored in
/// the vendor config map, parsed by each vendor crate.
///
/// The `managed` field controls whether this TV participates in automation
/// (boot WoL, session events, shutdown). Unmanaged TVs remain available for
/// manual control via the tray and CLI but are skipped by all event-driven paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvConfig {
    /// Stable identifier for this TV — RFC 4122 UUID v4
    /// generated at pair time. Decoupled from `name` so renames
    /// don't churn cross-references (profile assignments, D-Bus
    /// paths, `primary_input` keying, etc.). Matches
    /// NetworkManager's connection-uuid pattern.
    ///
    /// Serialized as the canonical hyphenated form
    /// (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`) in TOML; D-Bus
    /// paths substitute hyphens with underscores via the existing
    /// [`tv_path`](crate::dbus_paths::tv_path) helper. Tests
    /// needing deterministic UUIDs use
    /// [`with_uuid`](Self::with_uuid).
    pub uuid: Uuid,

    /// Display name.
    pub name: String,

    /// Vendor/backend to use for this TV (e.g., "lg", "samsung").
    pub vendor: String,

    /// The input this PC is connected to (e.g., "HDMI_1", "HDMI_2").
    ///
    /// Vendor-agnostic; not all inputs are HDMI (could be DVI, AV, coaxial,
    /// or even a WebOS app ID). Used for input-aware automation gating
    /// (see `automate_when` in `SystemProfileConfig`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_input: Option<String>,

    /// Whether this TV participates in automated management.
    ///
    /// When `false`, the TV is known to the daemon but skipped for all
    /// automation (boot WoL, session events, shutdown). Manual control via
    /// the tray and CLI still works.
    pub managed: bool,

    /// Per-TV profile assignments.
    ///
    /// `profile.system` selects a system profile for daemon lifecycle
    /// events by UUID. Defaults to [`DEFAULT_SYSTEM_PROFILE_UUID`].
    #[serde(default)]
    pub profile: TvProfileAssignment,

    /// Vendor-specific configuration.
    ///
    /// This captures vendor-specific fields (e.g., `lg = { ip = "...", client_key = "..." }`).
    /// Use `get_vendor_config::<T>()` to deserialize into a typed struct.
    #[serde(flatten)]
    pub vendor_config: HashMap<String, toml::Value>,
}

impl TvConfig {
    /// Create a new TV configuration with a freshly-generated UUID.
    ///
    /// `managed` controls whether this TV participates in automation (boot WoL,
    /// session events, shutdown). Pass `true` for normal TVs; `false` to register
    /// the TV for manual control only. There is no default — callers must be explicit.
    ///
    /// For tests that need a deterministic UUID, use
    /// [`with_uuid`](Self::with_uuid).
    pub fn new(name: impl Into<String>, vendor: impl Into<String>, managed: bool) -> Self {
        Self::with_uuid(Uuid::new_v4(), name, vendor, managed)
    }

    /// Create a TV configuration with an explicit UUID. Used by the
    /// pair flow (when `AddTv` provides the UUID) and by tests that
    /// need stable identifiers across runs.
    pub fn with_uuid(
        uuid: Uuid,
        name: impl Into<String>,
        vendor: impl Into<String>,
        managed: bool,
    ) -> Self {
        Self {
            uuid,
            name: name.into(),
            vendor: vendor.into(),
            primary_input: None,
            managed,
            profile: TvProfileAssignment::default(),
            vendor_config: HashMap::new(),
        }
    }

    /// Set the primary input (the port this PC is connected to).
    pub fn with_primary_input(mut self, input: impl Into<String>) -> Self {
        self.primary_input = Some(input.into());
        self
    }

    /// Set vendor-specific configuration.
    ///
    /// The config must be serializable to TOML. It will be stored under
    /// the vendor name key (e.g., `lg = { ... }`).
    pub fn with_vendor_config<T: Serialize>(mut self, config: &T) -> Self {
        if let Ok(value) = toml::Value::try_from(config) {
            self.vendor_config.insert(self.vendor.clone(), value);
        }
        self
    }

    /// Get the raw vendor config value for passing to connectors.
    ///
    /// Returns the TOML value for this TV's vendor section, if present.
    /// This is the transport-agnostic way to pass config to a `TvConnector`.
    pub fn get_raw_vendor_value(&self) -> Option<&toml::Value> {
        self.vendor_config.get(&self.vendor)
    }

    /// Get vendor-specific configuration.
    ///
    /// Deserializes the vendor config into the specified type.
    /// Returns `None` if the vendor config is missing or cannot be deserialized.
    pub fn get_vendor_config<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        self.vendor_config
            .get(&self.vendor)
            .and_then(|v| v.clone().try_into().ok())
    }
}

// =============================================================================
// SystemConfig (aka Config)
// =============================================================================

/// Discovery-related daemon settings.
///
/// Lives under `[discovery]` in the system TOML. All fields are
/// optional with `#[serde(default)]` so existing configs without the
/// section continue to load unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    /// Source UDP port to bind for SSDP M-SEARCH. `None` (or omitted)
    /// lets the OS pick an ephemeral port — the standard
    /// SSDP-client behavior, which forces an overly broad
    /// `accept any UDP from subnet` firewall rule because TV
    /// replies arrive at an unpredictable destination port. Setting
    /// a specific port (e.g. `1901`) lets a firewall accept a
    /// single `--dport` rule. See `docs/firewall.md` for the rule
    /// recipes per distro.
    #[serde(default)]
    pub bind_port: Option<u16>,

    /// Per-iteration timeout for the streaming discovery loop used by
    /// `Manager1.StartDiscovery`. Each iteration sends one M-SEARCH
    /// burst, waits for responses up to this duration, then starts the
    /// next iteration. Smaller values are more responsive to a TV
    /// being powered on mid-session; larger values reduce
    /// steady-state multicast traffic. `None` uses the default of
    /// 8000 ms.
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
}

impl DiscoveryConfig {
    /// Effective polling interval for streaming discovery iterations.
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.poll_interval_ms.unwrap_or(8000))
    }
}

/// System-level configuration (daemon-owned).
///
/// Contains TV definitions, vendor credentials, system profiles for daemon
/// lifecycle events, and user profile defaults inherited by users.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemConfig {
    /// System-level settings (daemon lifecycle profiles).
    #[serde(default)]
    pub system: SystemSection,

    /// User-level settings (session event profiles).
    ///
    /// In the system config, `[user.profiles.default]` is the admin baseline
    /// inherited by users who have not created their own config file.
    #[serde(default)]
    pub user: UserSection,

    /// Discovery-related daemon settings (SSDP bind port, etc.).
    #[serde(default)]
    pub discovery: DiscoveryConfig,

    /// Configured TVs.
    #[serde(default)]
    pub tv: Vec<TvConfig>,
}

impl SystemConfig {
    /// Load configuration from the default system path (`/var/lib/tisvlay/config.toml`).
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::system_config_path())
    }

    /// Load configuration from a specific path.
    ///
    /// Validates TV IDs and ensures the reserved default profile
    /// exists in each section. Missing file → bare default config
    /// with reserved defaults materialized.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            let mut config = Self::default();
            config.system.ensure_default();
            config.user.ensure_default();
            return Ok(config);
        }

        let content = std::fs::read_to_string(path)?;
        let mut config: SystemConfig = toml::from_str(&content)?;
        config.validate_tv_ids()?;
        config.system.ensure_default();
        config.user.ensure_default();
        Ok(config)
    }

    /// Save configuration to the default system path (`/var/lib/tisvlay/config.toml`).
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::system_config_path())
    }

    /// Save configuration to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content =
            toml::to_string_pretty(self).map_err(|e| Error::Io(std::io::Error::other(e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Get the system-wide configuration file path.
    ///
    /// Returns `/var/lib/tisvlay/config.toml`.
    pub fn system_config_path() -> PathBuf {
        PathBuf::from("/var/lib/tisvlay/config.toml")
    }

    /// Get the daemon's user-mode config file path (`tisvlayd --session`).
    ///
    /// Returns `~/.config/tisvlay/config.toml` via XDG. Used when the
    /// daemon runs on the session bus for development without `--config`.
    /// Distinct from [`UserConfig::config_path`] (which resolves the
    /// *session service's* file) — daemon and session service own
    /// different files in the user domain.
    pub fn user_config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "tisvlay").ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            ))
        })?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Validate that all TV UUIDs are unique. Per-TV format
    /// validity is handled by serde during deserialization (the
    /// `Uuid` type rejects malformed values), so this only catches
    /// the multi-entry collision case.
    pub fn validate_tv_ids(&self) -> Result<()> {
        let mut seen = std::collections::HashMap::<Uuid, usize>::new();
        for (i, tv) in self.tv.iter().enumerate() {
            if let Some(&prev) = seen.get(&tv.uuid) {
                return Err(Error::ConfigValidation(format!(
                    "Duplicate TV UUID '{}' (entries {} and {})",
                    tv.uuid,
                    prev + 1,
                    i + 1,
                )));
            }
            seen.insert(tv.uuid, i);
        }
        Ok(())
    }

    /// Find a TV by UUID string (canonical hyphenated form).
    pub fn find_tv(&self, id: &str) -> Option<&TvConfig> {
        let target = Uuid::parse_str(id).ok()?;
        self.tv.iter().find(|tv| tv.uuid == target)
    }

    /// Find a TV by UUID string (mutable).
    pub fn find_tv_mut(&mut self, id: &str) -> Option<&mut TvConfig> {
        let target = Uuid::parse_str(id).ok()?;
        self.tv.iter_mut().find(|tv| tv.uuid == target)
    }

    /// Add or update a TV configuration.
    pub fn upsert_tv(&mut self, tv: TvConfig) {
        if let Some(existing) = self.tv.iter_mut().find(|t| t.uuid == tv.uuid) {
            *existing = tv;
        } else {
            self.tv.push(tv);
        }
    }

    /// Remove a TV by UUID string.
    pub fn remove_tv(&mut self, id: &str) -> bool {
        let Ok(target) = Uuid::parse_str(id) else {
            return false;
        };
        let len = self.tv.len();
        self.tv.retain(|tv| tv.uuid != target);
        self.tv.len() < len
    }

    /// Resolve the effective system profile for a TV.
    ///
    /// Resolution chain:
    /// 1. TV's assigned system profile (by UUID)
    /// 2. [`DEFAULT_SYSTEM_PROFILE_UUID`] profile body
    /// 3. Hard-coded defaults
    pub fn resolve_system_profile(&self, tv: &TvConfig) -> ResolvedSystemProfile {
        self.system.resolve_profile_by_uuid(tv.profile.system)
    }
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
    /// TV UUID (matches `TvConfig.uuid` from the system config).
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
        let config: UserConfig = toml::from_str(&content)?;
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
    /// Uses XDG directories: `~/.config/tisvlay/session.toml`. Per the
    /// per-peer file naming convention, the session service owns
    /// `session.toml` in the user domain (the daemon's user-mode file
    /// is `config.toml`, the tray's is `tray.toml`).
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
                // before reaching the system default, so user-level overrides are honoured.
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

    /// Test struct for LG vendor config (simulates what tisvlay-lg would define).
    /// Includes connection parameters (ip/mac) since they're vendor-specific.
    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    struct TestLgConfig {
        ip: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mac: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        client_key: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default_input: Option<String>,
    }

    impl TestLgConfig {
        fn new(ip: impl Into<String>) -> Self {
            Self {
                ip: ip.into(),
                mac: None,
                client_key: None,
                default_input: None,
            }
        }

        fn with_mac(mut self, mac: impl Into<String>) -> Self {
            self.mac = Some(mac.into());
            self
        }
    }

    // =========================================================================
    // SystemConfig tests
    // =========================================================================

    #[test]
    fn test_default_config() {
        let config = SystemConfig::default();
        assert!(config.tv.is_empty());
        assert!(config.system.profiles.is_empty());
        assert!(config.user.profiles.is_empty());
        // With no profiles defined, resolution should give hard-coded defaults
        let tv = TvConfig::new("Test", "lg", true);
        let resolved = config.resolve_system_profile(&tv);
        assert_eq!(resolved.on_startup, SessionAction::PowerOn);
        assert_eq!(resolved.on_shutdown, SessionAction::PowerOff);
        assert_eq!(resolved.on_reboot, SessionAction::None);
        assert_eq!(resolved.on_no_session, SessionAction::None);
    }

    #[test]
    fn test_parse_config_with_profiles_and_tvs() {
        // Config with user profiles and TVs — should parse cleanly
        let gaming_uuid = "a1b2c3d4-0000-0000-0000-000000000020";
        let toml_str = format!(
            r#"
[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
on_session_lock = "screen_off"
on_session_unlock = "screen_on"

[user.profiles."{gaming_uuid}"]
name = "gaming"
on_session_lock = "none"
idle_timeout_seconds = 0

[[tv]]
uuid = "11111111-1111-1111-1111-111111111111"
name = "Living Room TV"
vendor = "lg"
primary_input = "HDMI_1"
managed = true

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123"
default_input = "HDMI_1"
"#
        );

        let config: SystemConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].vendor, "lg");
        assert_eq!(config.tv[0].primary_input, Some("HDMI_1".to_string()));

        // Check vendor-specific config
        let lg: TestLgConfig = config.tv[0].get_vendor_config().unwrap();
        assert_eq!(lg.ip, "192.168.1.100");
        assert_eq!(lg.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(lg.client_key, Some("abc123".to_string()));
        assert_eq!(lg.default_input, Some("HDMI_1".to_string()));

        // Check user profiles
        assert_eq!(config.user.profiles.len(), 2);
        let gaming = config
            .user
            .find_profile_by_name("gaming")
            .map(|(_, body)| body)
            .unwrap();
        assert_eq!(gaming.on_session_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));
    }

    #[test]
    fn test_parse_new_system_config() {
        // New-style system config with system and user profile sections
        let toml_str = r#"
[system.profiles."00000000-0000-0000-0000-000000005159"]
name = "default"
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_no_session = "screen_on"

[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
on_session_lock = "screen_off"
idle_timeout_seconds = 300

[[tv]]
uuid = "11111111-1111-1111-1111-111111111111"
name = "Living Room G4"
vendor = "lg"
primary_input = "HDMI_1"
managed = true

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();

        // Check system profile
        let sys_default = config
            .system
            .profiles
            .get(&DEFAULT_SYSTEM_PROFILE_UUID)
            .unwrap();
        assert_eq!(sys_default.on_startup, Some(SessionAction::PowerOn));
        assert_eq!(sys_default.on_shutdown, Some(SessionAction::PowerOff));
        assert_eq!(sys_default.on_reboot, Some(SessionAction::None));
        assert_eq!(sys_default.on_no_session, Some(SessionAction::ScreenOn));

        // Check user profile
        let user_default = config
            .user
            .profiles
            .get(&DEFAULT_USER_PROFILE_UUID)
            .unwrap();
        assert_eq!(user_default.on_session_lock, Some(SessionAction::ScreenOff));
        assert_eq!(user_default.idle_timeout_seconds, Some(300));
        // Unspecified field
        assert_eq!(user_default.on_greeter_wake, None);
    }

    #[test]
    fn test_single_file_config_with_both_profiles() {
        // Single-file mode: system and user profiles + TV definitions with
        // both profile.system and profile.user on [[tv]] entries.
        // Both SystemConfig and UserConfig can parse the same TOML:
        // - SystemConfig sees profile.system, ignores profile.user
        // - UserConfig sees profile.user, ignores name/vendor/etc.
        let guest_uuid = "a1b2c3d4-0000-0000-0000-000000000010";
        let gaming_uuid = "a1b2c3d4-0000-0000-0000-000000000020";
        let toml_str = format!(
            r#"
[system.profiles."00000000-0000-0000-0000-000000005159"]
name = "default"
on_startup = "power_on"
on_shutdown = "power_off"

[system.profiles."{guest_uuid}"]
name = "guest"
on_startup = "none"
on_shutdown = "screen_off"

[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
on_session_lock = "screen_off"

[user.profiles."{gaming_uuid}"]
name = "gaming"
on_session_lock = "none"
idle_timeout_seconds = 0

[[tv]]
uuid = "11111111-1111-1111-1111-111111111111"
name = "Living Room"
vendor = "lg"
managed = true
profile.system = "00000000-0000-0000-0000-000000005159"
profile.user = "{gaming_uuid}"

[tv.lg]
ip = "192.168.1.100"

[[tv]]
uuid = "22222222-2222-2222-2222-222222222222"
name = "Guest Room"
vendor = "lg"
managed = true
profile.system = "{guest_uuid}"

[tv.lg]
ip = "192.168.1.101"
"#
        );

        // Parse as SystemConfig
        let config: SystemConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.tv.len(), 2);

        let guest_uuid_v: Uuid = guest_uuid.parse().unwrap();
        let gaming_uuid_v: Uuid = gaming_uuid.parse().unwrap();

        let living = config.tv.iter().find(|t| t.name == "Living Room").unwrap();
        assert_eq!(living.profile.system, DEFAULT_SYSTEM_PROFILE_UUID);

        let guest = config.tv.iter().find(|t| t.name == "Guest Room").unwrap();
        assert_eq!(guest.profile.system, guest_uuid_v);

        // System profile resolution works
        let resolved = config.resolve_system_profile(living);
        assert_eq!(resolved.on_startup, SessionAction::PowerOn);

        let resolved = config.resolve_system_profile(guest);
        assert_eq!(resolved.on_startup, SessionAction::None);
        assert_eq!(resolved.on_shutdown, SessionAction::ScreenOff);

        // Parse the SAME TOML as UserConfig — UserTvConfig ignores unknown
        // fields (name, vendor, managed, lg, profile.system)
        // and picks up profile.user.
        let user_config: UserConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(user_config.tv.len(), 2);

        let living_user = user_config
            .tv
            .iter()
            .find(|t| t.uuid == "11111111-1111-1111-1111-111111111111")
            .unwrap();
        assert_eq!(living_user.profile.user, gaming_uuid_v);

        let guest_user = user_config
            .tv
            .iter()
            .find(|t| t.uuid == "22222222-2222-2222-2222-222222222222")
            .unwrap();
        // profile.user not specified, defaults to DEFAULT_USER_PROFILE_UUID.
        assert_eq!(guest_user.profile.user, DEFAULT_USER_PROFILE_UUID);
    }

    #[test]
    fn test_vendor_config() {
        let lg_config = TestLgConfig {
            ip: "192.168.1.1".to_string(),
            mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
            client_key: Some("test_key".to_string()),
            default_input: Some("HDMI_1".to_string()),
        };

        let tv = TvConfig::new("Test", "lg", true).with_vendor_config(&lg_config);

        assert_eq!(tv.vendor, "lg");

        let extracted: TestLgConfig = tv.get_vendor_config().unwrap();
        assert_eq!(extracted.ip, "192.168.1.1");
        assert_eq!(extracted.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(extracted, lg_config);
    }

    #[test]
    fn test_find_tv() {
        let tv1_uuid = Uuid::from_u128(1);
        let tv2_uuid = Uuid::from_u128(2);
        let absent_uuid = Uuid::from_u128(99);

        let mut config = SystemConfig::default();
        config.tv.push(
            TvConfig::with_uuid(tv1_uuid, "TV 1", "lg", true).with_vendor_config(
                &TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:01"),
            ),
        );
        config.tv.push(
            TvConfig::with_uuid(tv2_uuid, "TV 2", "lg", true).with_vendor_config(
                &TestLgConfig::new("192.168.1.2").with_mac("AA:BB:CC:DD:EE:02"),
            ),
        );

        assert!(config.find_tv(&tv1_uuid.to_string()).is_some());
        assert!(config.find_tv(&absent_uuid.to_string()).is_none());
        // Non-UUID strings are no longer valid lookups.
        assert!(config.find_tv("tv1").is_none());
    }

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

    #[test]
    fn test_save_does_not_auto_insert_default_profile() {
        // save_to must not silently add profiles when no profiles exist.
        use std::io::Read;

        let mut config = SystemConfig::default();
        config.tv.push(
            TvConfig::new("Test TV", "lg", true).with_vendor_config(
                &TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:FF"),
            ),
        );
        assert!(config.user.profiles.is_empty());
        assert!(config.system.profiles.is_empty());

        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("tisvlay_test_config_b.toml");
        config.save_to(&temp_file).unwrap();

        let mut content = String::new();
        std::fs::File::open(&temp_file)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let saved_config: SystemConfig = toml::from_str(&content).unwrap();
        assert!(
            saved_config.user.profiles.is_empty(),
            "save_to must not auto-insert user profiles"
        );
        assert!(
            saved_config.system.profiles.is_empty(),
            "save_to must not auto-insert system profiles"
        );

        let _ = std::fs::remove_file(temp_file);
    }

    #[test]
    fn test_save_preserves_existing_profiles() {
        use std::io::Read;

        let mut config = SystemConfig::default();
        let custom_uuid = Uuid::new_v4();
        let guest_uuid = Uuid::new_v4();
        config.user.profiles.insert(
            custom_uuid,
            UserProfileConfig {
                name: "custom".to_string(),
                on_session_lock: Some(SessionAction::PowerOff),
                ..UserProfileConfig::default()
            },
        );
        config.system.profiles.insert(
            guest_uuid,
            SystemProfileConfig {
                name: "guest".to_string(),
                on_shutdown: Some(SessionAction::ScreenOff),
                ..SystemProfileConfig::default()
            },
        );

        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("tisvlay_test_config2_b.toml");
        config.save_to(&temp_file).unwrap();

        let mut content = String::new();
        std::fs::File::open(&temp_file)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let saved_config: SystemConfig = toml::from_str(&content).unwrap();

        assert!(saved_config.user.profiles.contains_key(&custom_uuid));
        assert!(
            !saved_config
                .user
                .profiles
                .contains_key(&DEFAULT_USER_PROFILE_UUID)
        );
        assert!(saved_config.system.profiles.contains_key(&guest_uuid));

        let _ = std::fs::remove_file(temp_file);
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
        assert_eq!(resolved.on_no_session, SessionAction::None);
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
            on_startup: Some(SessionAction::PowerOn),
            on_shutdown: Some(SessionAction::PowerOff),
            ..SystemProfileConfig::default()
        };

        let override_profile = SystemProfileConfig {
            on_shutdown: Some(SessionAction::ScreenOff),
            ..SystemProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_startup, Some(SessionAction::PowerOn)); // from base
        assert_eq!(merged.on_shutdown, Some(SessionAction::ScreenOff)); // overridden
    }

    #[test]
    fn test_resolve_system_profile_per_tv() {
        let mut config = SystemConfig::default();
        let guest_uuid = Uuid::new_v4();
        config.system.profiles.insert(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileConfig {
                name: "default".to_string(),
                on_startup: Some(SessionAction::PowerOn),
                on_shutdown: Some(SessionAction::PowerOff),
                ..SystemProfileConfig::default()
            },
        );
        config.system.profiles.insert(
            guest_uuid,
            SystemProfileConfig {
                name: "guest".to_string(),
                on_startup: Some(SessionAction::None),
                on_shutdown: Some(SessionAction::ScreenOff),
                ..SystemProfileConfig::default()
            },
        );

        // TV with default profile
        let tv_default = TvConfig::new("Living Room", "lg", true);
        let resolved = config.resolve_system_profile(&tv_default);
        assert_eq!(resolved.on_startup, SessionAction::PowerOn);
        assert_eq!(resolved.on_shutdown, SessionAction::PowerOff);

        // TV with guest profile
        let mut tv_guest = TvConfig::new("Guest Room", "lg", true);
        tv_guest.profile.system = guest_uuid;
        let resolved = config.resolve_system_profile(&tv_guest);
        assert_eq!(resolved.on_startup, SessionAction::None);
        assert_eq!(resolved.on_shutdown, SessionAction::ScreenOff);
        // on_reboot not set in guest, falls through to default
        assert_eq!(resolved.on_reboot, SessionAction::None); // from default profile
    }

    #[test]
    fn test_resolve_system_profile_nonexistent_falls_to_default() {
        let mut config = SystemConfig::default();
        config.system.profiles.insert(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileConfig {
                name: "default".to_string(),
                on_startup: Some(SessionAction::PowerOn),
                ..SystemProfileConfig::default()
            },
        );

        let mut tv = TvConfig::new("Test", "lg", true);
        // Stale UUID with no body — should fall through to default body.
        tv.profile.system = Uuid::new_v4();
        let resolved = config.resolve_system_profile(&tv);
        assert_eq!(resolved.on_startup, SessionAction::PowerOn); // from default
    }

    #[test]
    fn test_tv_profile_assignment_serialization() {
        let guest_uuid = "a1b2c3d4-0000-0000-0000-000000000010";
        let toml_str = format!(
            r#"
[system.profiles."00000000-0000-0000-0000-000000005159"]
name = "default"
on_startup = "power_on"

[[tv]]
uuid = "11111111-1111-1111-1111-111111111111"
name = "Living Room"
vendor = "lg"
managed = true
profile.system = "{guest_uuid}"

[tv.lg]
ip = "192.168.1.100"
"#
        );

        let config: SystemConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            config.tv[0].profile.system,
            guest_uuid.parse::<Uuid>().unwrap()
        );
    }

    #[test]
    fn test_tv_profile_assignment_default() {
        let toml_str = r#"
[[tv]]
uuid = "33333333-3333-3333-3333-333333333333"
name = "Test"
vendor = "lg"
managed = true

[tv.lg]
ip = "192.168.1.1"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        // Defaults to DEFAULT_SYSTEM_PROFILE_UUID when omitted.
        assert_eq!(config.tv[0].profile.system, DEFAULT_SYSTEM_PROFILE_UUID);
    }

    // =========================================================================
    // ResolvedUserProfile tests
    // =========================================================================

    #[test]
    fn test_resolved_profile_default() {
        let resolved = ResolvedUserProfile::default();
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn);
        assert_eq!(resolved.on_greeter_wake, SessionAction::ScreenOn);
        assert_eq!(resolved.idle_timeout_seconds, 0);
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
            on_session_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let override_profile = UserProfileConfig {
            on_session_lock: Some(SessionAction::None),
            // idle_timeout_seconds not specified — should inherit from base
            ..UserProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_session_lock, Some(SessionAction::None));
        assert_eq!(merged.idle_timeout_seconds, Some(300)); // inherited
    }

    #[test]
    fn test_merge_with_base_empty_override() {
        let base = UserProfileConfig::full_default();
        let empty = UserProfileConfig::default(); // all None

        let merged = empty.merge_with_base(&base);
        // All fields should come from base
        assert_eq!(merged.on_session_lock, base.on_session_lock);
        assert_eq!(merged.idle_timeout_seconds, base.idle_timeout_seconds);
    }

    #[test]
    fn test_merge_with_base_full_override() {
        let base = UserProfileConfig::full_default();
        let full_override = UserProfileConfig {
            name: String::new(),
            on_session_lock: Some(SessionAction::None),
            on_session_unlock: Some(SessionAction::None),
            on_greeter_wake: Some(SessionAction::None),
            idle_timeout_seconds: Some(999),
            on_idle_timeout: Some(SessionAction::PowerOff),
            on_idle_resume: Some(SessionAction::None),
        };

        let merged = full_override.merge_with_base(&base);
        assert_eq!(merged.on_session_lock, Some(SessionAction::None));
        assert_eq!(merged.idle_timeout_seconds, Some(999));
        assert_eq!(merged.on_idle_timeout, Some(SessionAction::PowerOff));
    }

    #[test]
    fn test_resolve_fills_nones() {
        let partial = UserProfileConfig {
            on_session_lock: Some(SessionAction::None),
            // Everything else is None
            ..UserProfileConfig::default()
        };

        let resolved = partial.resolve();
        assert_eq!(resolved.on_session_lock, SessionAction::None); // from partial
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn); // hard-coded default
        assert_eq!(resolved.idle_timeout_seconds, 0); // hard-coded default
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
idle_timeout_seconds = 600

[user.profiles."{gaming_uuid}"]
name = "gaming"
on_session_lock = "none"
idle_timeout_seconds = 0

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
            on_session_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None),
                // idle_timeout_seconds not specified — should inherit from system
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living_room", gaming_uuid);

        let resolved = user.resolve_user_profile("living_room", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::None); // user gaming
        assert_eq!(resolved.idle_timeout_seconds, 300); // inherited from system
    }

    #[test]
    fn test_user_config_resolve_user_profile_no_assignment() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let user = UserConfig::default(); // no profiles, no TV assignments

        // TV without assignment uses system default
        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.idle_timeout_seconds, 300);
    }

    #[test]
    fn test_user_config_resolve_user_profile_user_default_overrides_system() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                idle_timeout_seconds: Some(600), // user overrides system
                ..UserProfileConfig::default()
            },
        );

        // TV without explicit assignment uses user's default profile
        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff); // from system
        assert_eq!(resolved.idle_timeout_seconds, 600); // user override
    }

    #[test]
    fn test_user_config_resolve_user_profile_named_inherits_user_default() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(100),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        // User's default profile overrides system idle timeout
        user.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                idle_timeout_seconds: Some(600),
                ..UserProfileConfig::default()
            },
        );
        // User's gaming profile overrides screen lock only
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living_room", gaming_uuid);

        let resolved = user.resolve_user_profile("living_room", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::None); // gaming
        assert_eq!(resolved.idle_timeout_seconds, 600); // user default (not system 100)
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
                on_session_lock: Some(SessionAction::None),
                idle_timeout_seconds: Some(0),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living_room", gaming_uuid);

        let serialized = toml::to_string_pretty(&user).unwrap();
        let deserialized: UserConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.user.profiles.len(), 1);
        let gaming = deserialized.user.profiles.get(&gaming_uuid).unwrap();
        assert_eq!(gaming.on_session_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));
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
        let custom_sys_uuid = "a1b2c3d4-0000-0000-0000-000000000031";
        let custom_user_uuid = "a1b2c3d4-0000-0000-0000-000000000032";
        let toml_str = format!(
            r#"
[system.profiles."{custom_sys_uuid}"]
name = "custom"
on_sleep = "none"
on_wake = "screen_on"

[user.profiles."{custom_user_uuid}"]
name = "custom"
on_session_lock = "power_off"
on_session_unlock = "power_on"
on_greeter_wake = "none"
idle_timeout_seconds = 300
on_idle_timeout = "screen_off"

[[tv]]
uuid = "33333333-3333-3333-3333-333333333333"
name = "Test"
vendor = "lg"
managed = true

[tv.lg]
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:FF"
"#
        );

        let config: SystemConfig = toml::from_str(&toml_str).unwrap();

        // Check system profile fields
        let sys_profile = config
            .system
            .find_profile_by_name("custom")
            .map(|(_, body)| body.clone())
            .unwrap_or_else(SystemProfileConfig::full_default);
        assert_eq!(sys_profile.on_sleep, Some(SessionAction::None));
        assert_eq!(sys_profile.on_wake, Some(SessionAction::ScreenOn));

        // Check user profile fields
        let profile = config
            .user
            .find_profile_by_name("custom")
            .map(|(_, body)| body.clone())
            .unwrap_or_else(UserProfileConfig::full_default);
        assert_eq!(profile.on_session_lock, Some(SessionAction::PowerOff));
        assert_eq!(profile.on_session_unlock, Some(SessionAction::PowerOn));
        assert_eq!(profile.on_greeter_wake, Some(SessionAction::None));
        assert_eq!(profile.idle_timeout_seconds, Some(300));
        assert_eq!(profile.on_idle_timeout, Some(SessionAction::ScreenOff));

        // Resolve to concrete values
        let resolved = profile.resolve();
        assert_eq!(resolved.on_session_lock, SessionAction::PowerOff);
        assert_eq!(resolved.idle_timeout_seconds, 300);
    }

    // =========================================================================
    // System + User profile merging (simulates session service behavior)
    // =========================================================================

    #[test]
    fn test_system_profiles_parsed_from_system_config() {
        // System config has multiple user profiles — session service should see all of them
        let foo_uuid = "a1b2c3d4-0000-0000-0000-000000000040";
        let toml_str = format!(
            r#"
[system.profiles."00000000-0000-0000-0000-000000005159"]
name = "default"
on_startup = "power_on"

[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
on_session_lock = "screen_off"
idle_timeout_seconds = 300

[user.profiles."{foo_uuid}"]
name = "foo"
on_session_lock = "none"
idle_timeout_seconds = 0

[[tv]]
uuid = "22222222-2222-2222-2222-222222222222"
name = "Office"
vendor = "lg"
managed = true

[tv.lg]
ip = "192.168.1.1"
"#
        );

        let config: SystemConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert!(
            config
                .user
                .profiles
                .contains_key(&DEFAULT_USER_PROFILE_UUID)
        );
        assert!(config.user.find_profile_by_name("foo").is_some());

        let foo = config
            .user
            .find_profile_by_name("foo")
            .map(|(_, body)| body)
            .unwrap();
        assert_eq!(foo.on_session_lock, Some(SessionAction::None));
        assert_eq!(foo.idle_timeout_seconds, Some(0));
    }

    #[test]
    fn test_merged_profile_list_system_only() {
        // When user has no config, all profiles come from system config
        let system_profiles: HashMap<String, UserProfileConfig> = [
            (
                "default".to_string(),
                UserProfileConfig {
                    on_session_lock: Some(SessionAction::ScreenOff),
                    ..UserProfileConfig::default()
                },
            ),
            (
                "foo".to_string(),
                UserProfileConfig {
                    on_session_lock: Some(SessionAction::None),
                    ..UserProfileConfig::default()
                },
            ),
        ]
        .into_iter()
        .collect();

        let user_config = UserConfig::default(); // empty

        // Merge: system profiles as base, user profiles override.
        // Keyed by body name — same shape the session_dbus list-merge uses.
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
                    on_session_lock: Some(SessionAction::ScreenOff),
                    idle_timeout_seconds: Some(300),
                    ..UserProfileConfig::default()
                },
            ),
            (
                "foo".to_string(),
                UserProfileConfig {
                    name: "foo".to_string(),
                    on_session_lock: Some(SessionAction::None),
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
                idle_timeout_seconds: Some(600), // user overrides
                ..UserProfileConfig::default()
            },
        );
        user_config.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );

        // Merge: system first, then user overrides. Body name is the
        // wire-shape key (matches session_dbus's list_user_profiles merge).
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

        // "default" should be the user's version (idle_timeout_seconds = 600)
        let default = merged.get("default").unwrap();
        assert_eq!(default.idle_timeout_seconds, Some(600));

        // "foo" comes from system
        let foo = merged.get("foo").unwrap();
        assert_eq!(foo.on_session_lock, Some(SessionAction::None));
    }

    #[test]
    fn test_user_config_ignores_unknown_toml_fields() {
        // UserConfig should parse even if the TOML has extra fields
        let gaming_uuid = "a1b2c3d4-0000-0000-0000-000000000020";
        let user_only_toml = format!(
            r#"
[user.profiles."00000000-0000-0000-0000-00000000515a"]
name = "default"
idle_timeout_seconds = 600

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
            on_idle_resume: Some(SessionAction::None),
            ..UserProfileConfig::default()
        };
        let resolved = profile.resolve();
        assert_eq!(resolved.on_idle_resume, SessionAction::None);

        // Inherits from system default if not set in user profile
        let system = UserProfileConfig {
            on_idle_resume: Some(SessionAction::PowerOn),
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
            on_session_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: "default".to_string(),
                idle_timeout_seconds: Some(600),
                ..UserProfileConfig::default()
            },
        );
        // Stale UUID with no body — resolution falls through to user default.
        user.set_tv_user_profile("living_room", Uuid::new_v4());

        let resolved = user.resolve_user_profile("living_room", &system_default);
        assert_eq!(resolved.idle_timeout_seconds, 600);
        assert_eq!(resolved.on_session_lock, SessionAction::ScreenOff);
    }

    #[test]
    fn test_resolve_user_profile_with_system_profiles_as_base() {
        let system_default = UserProfileConfig {
            on_session_lock: Some(SessionAction::ScreenOff),
            on_session_unlock: Some(SessionAction::ScreenOn),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        let gaming_uuid = Uuid::new_v4();
        user.user.profiles.insert(
            gaming_uuid,
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("office", gaming_uuid);

        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_session_lock, SessionAction::None);
        assert_eq!(resolved.idle_timeout_seconds, 300);
        assert_eq!(resolved.on_session_unlock, SessionAction::ScreenOn);

        let resolved_unknown = user.resolve_user_profile("other_tv", &system_default);
        assert_eq!(resolved_unknown.on_session_lock, SessionAction::ScreenOff);
        assert_eq!(resolved_unknown.idle_timeout_seconds, 300);
    }

    // =========================================================================
    // TV UUID validation
    // =========================================================================

    #[test]
    fn test_validate_rejects_duplicate_ids() {
        let mut config = SystemConfig::default();
        let shared = Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111);
        config
            .tv
            .push(TvConfig::with_uuid(shared, "Living Room", "lg", true));
        config
            .tv
            .push(TvConfig::with_uuid(shared, "Office", "lg", true));
        let err = config.validate_tv_ids().unwrap_err();
        assert!(
            err.to_string().contains("Duplicate"),
            "error should say duplicate: {}",
            err
        );
    }

    #[test]
    fn test_validate_accepts_unique_uuids() {
        let mut config = SystemConfig::default();
        config.tv.push(TvConfig::new("Living Room", "lg", true));
        config.tv.push(TvConfig::new("Office", "lg", true));
        config.validate_tv_ids().unwrap();
    }

    // =========================================================================
    // SystemProfileBody / UserProfileBody dict-derive round-trip
    // =========================================================================
    //
    // The wire shape rides on `Option<T>::None` ⇔ absent-key + `name`
    // always-present. These tests pin both halves so a future zvariant or
    // zbus bump that changes the dict-derive's skip-if-None or
    // mandatory-field semantics fails loudly here instead of silently
    // breaking the inherit-vs-locally-set distinction the GUI relies on.

    use zbus::zvariant::{self, LE, serialized::Context, to_bytes};

    fn round_trip<T>(value: &T) -> T
    where
        T: zvariant::Type + serde::Serialize + for<'de> serde::Deserialize<'de> + std::fmt::Debug,
    {
        let ctx = Context::new_dbus(LE, 0);
        let encoded = to_bytes(ctx, value).expect("encode");
        let (decoded, _): (T, usize) = encoded.deserialize().expect("decode");
        decoded
    }

    #[test]
    fn system_profile_body_round_trip_full() {
        let body = SystemProfileBody {
            name: "office".into(),
            on_startup: Some(SessionAction::PowerOn),
            on_shutdown: Some(SessionAction::PowerOff),
            on_reboot: Some(SessionAction::None),
            on_no_session: Some(SessionAction::None),
            on_sleep: Some(SessionAction::PowerOff),
            on_wake: Some(SessionAction::PowerOn),
            automate_when: Some(AutomateWhen::Always),
        };

        let decoded = round_trip(&body);

        assert_eq!(decoded.name, body.name);
        assert_eq!(decoded.on_startup, body.on_startup);
        assert_eq!(decoded.automate_when, body.automate_when);
    }

    #[test]
    fn system_profile_body_round_trip_minimal_omits_none() {
        // Body with only `name` set; every optional field is None.
        // The dict-derive must serialize *only* the `name` key on the wire,
        // and the round-trip must come back with the same None fields.
        let body = SystemProfileBody {
            name: "guest".into(),
            ..Default::default()
        };

        let decoded = round_trip(&body);

        assert_eq!(decoded.name, "guest");
        assert!(decoded.on_startup.is_none());
        assert!(decoded.on_shutdown.is_none());
        assert!(decoded.automate_when.is_none());
    }

    #[test]
    fn user_profile_body_round_trip_with_numeric() {
        let body = UserProfileBody {
            name: "default".into(),
            on_session_lock: Some(SessionAction::ScreenOff),
            on_session_unlock: Some(SessionAction::ScreenOn),
            on_greeter_wake: None,
            on_idle_timeout: Some(SessionAction::ScreenOff),
            on_idle_resume: Some(SessionAction::ScreenOn),
            idle_timeout_seconds: Some(600),
        };

        let decoded = round_trip(&body);

        assert_eq!(decoded.idle_timeout_seconds, Some(600));
        assert!(decoded.on_greeter_wake.is_none());
        assert_eq!(decoded.on_idle_timeout, Some(SessionAction::ScreenOff));
    }

    #[test]
    fn system_profile_body_from_config_preserves_options() {
        let cfg = SystemProfileConfig {
            name: "office".to_string(),
            on_startup: Some(SessionAction::PowerOn),
            on_shutdown: None,
            on_reboot: Some(SessionAction::None),
            on_no_session: None,
            on_sleep: Some(SessionAction::PowerOff),
            on_wake: None,
            automate_when: Some(AutomateWhen::OnPrimary),
        };
        let body = SystemProfileBody::from_config(&cfg);
        assert_eq!(body.name, "office");
        assert_eq!(body.on_startup, cfg.on_startup);
        assert_eq!(body.on_shutdown, cfg.on_shutdown);
        assert_eq!(body.automate_when, cfg.automate_when);

        // Reverse: full round-trip including name.
        let back: SystemProfileConfig = (&body).into();
        assert_eq!(back.name, "office");
        assert_eq!(back.on_startup, cfg.on_startup);
        assert_eq!(back.on_shutdown, cfg.on_shutdown);
        assert_eq!(back.automate_when, cfg.automate_when);
    }

    #[test]
    fn system_profile_body_dict_signature_is_a_sv() {
        // Pin the wire signature so a future struct-variant addition to
        // `SessionAction` (which would silently change the variant signature
        // from "s" to "us" or similar) is caught at test time.
        assert_eq!(
            <SystemProfileBody as zvariant::Type>::SIGNATURE.to_string(),
            "a{sv}"
        );
        assert_eq!(
            <UserProfileBody as zvariant::Type>::SIGNATURE.to_string(),
            "a{sv}"
        );
    }
}
