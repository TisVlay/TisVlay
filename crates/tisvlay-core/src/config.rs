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
//! [system.profiles."00000000-0000-0000-0000-000000000001"]
//! name = "default"
//! on_startup = "power_on"
//! on_shutdown = "power_off"
//! on_reboot = "none"
//! on_no_session = "none"
//! on_sleep = "power_off"
//! on_wake = "power_on"
//!
//! [user.profiles."00000000-0000-0000-0000-000000000002"]
//! name = "default"
//! on_session_lock = "screen_off"
//! on_session_unlock = "screen_on"
//!
//! [[tv]]
//! uuid = "11111111-1111-1111-1111-111111111111"
//! name = "Living Room TV"
//! vendor = "lg"
//! primary_input = "HDMI_1"
//! profile.system = "00000000-0000-0000-0000-000000000001"
//!
//! [tv.lg]
//! ip = "192.168.1.100"
//! mac = "AA:BB:CC:DD:EE:FF"
//! client_key = "abc123"
//! ```
//!
//! ## User Config (`~/.config/tisvlay/config.toml`)
//!
//! Owned by each user's `tisvlay-session`. Contains profile definitions
//! and per-TV profile assignments. Optional — if absent, the session service
//! inherits the system default profile.
//!
//! ```toml
//! [user.profiles."00000000-0000-0000-0000-000000000002"]
//! name = "default"
//! idle_timeout_seconds = 600
//!
//! [user.profiles."a1b2c3d4-..."]
//! name = "gaming"
//! on_session_lock = "none"
//! idle_timeout_seconds = 0
//!
//! [[tv]]
//! uuid = "11111111-1111-1111-1111-111111111111"
//! profile.user = "a1b2c3d4-..."
//! ```

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use uuid::{Uuid, uuid};

/// Stable UUID for the reserved "default" system profile.
///
/// Hardcoded so `[[tv]]` entries can reference it by name (`"default"`)
/// regardless of whether the user has ever touched the GUI, and so the
/// D-Bus object path under `/org/tisvlay/SystemProfile/<UUID_>` for
/// the default profile stays consistent across daemon restarts.
/// Mirrors the way `00000000-0000-0000-0000-000000000000` is reserved
/// for the NIL UUID — this one is the first non-NIL sentinel.
pub const DEFAULT_SYSTEM_PROFILE_UUID: Uuid = uuid!("00000000-0000-0000-0000-000000000001");

/// Stable UUID for the reserved "default" user profile.
///
/// Same role as [`DEFAULT_SYSTEM_PROFILE_UUID`] on the user bus.
pub const DEFAULT_USER_PROFILE_UUID: Uuid = uuid!("00000000-0000-0000-0000-000000000002");

/// Name reserved for the default profile. Daemon refuses to create
/// or delete a profile with this name; only the default-UUID profile
/// may carry it. Kept in lockstep with the GUI's `(default)` badge.
pub const DEFAULT_PROFILE_NAME: &str = "default";

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

impl std::fmt::Display for SessionAction {
    /// Render as the serde `rename_all = "snake_case"` wire form so
    /// `SystemProfile1` / `UserProfile1` D-Bus properties report the
    /// same strings the on-disk TOML uses.
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub fn full_default() -> Self {
        let d = ResolvedSystemProfile::default();
        Self {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_startup: Some(d.on_startup),
            on_shutdown: Some(d.on_shutdown),
            on_reboot: Some(d.on_reboot),
            on_no_session: Some(d.on_no_session),
            on_sleep: Some(d.on_sleep),
            on_wake: Some(d.on_wake),
            automate_when: Some(d.automate_when),
        }
    }

    /// Materialized-but-empty default body — every field `None` (i.e.,
    /// inheriting from the hardcoded baseline). Used by
    /// [`SystemSection::ensure_default`] to guarantee the reserved
    /// default UUID always has a body on the bus without committing
    /// any opinion on the field values; the daemon's resolution walk
    /// falls through to [`ResolvedSystemProfile::default`] for every
    /// field. Lets the user customize the default via `Properties.Set`
    /// without first creating the section by hand on disk.
    pub fn empty_default() -> Self {
        Self {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..Self::default()
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`. Fields
    /// that are `Some` in `self` take precedence. Names don't merge —
    /// the returned body carries `self`'s name verbatim (callers
    /// reading the merged value already know which profile they
    /// asked about).
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserProfileConfig {
    /// Friendly name (same role as [`SystemProfileConfig::name`]).
    /// Identity is the containing-map's UUID key.
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
    /// or as the system default profile.
    pub fn full_default() -> Self {
        let d = ResolvedUserProfile::default();
        Self {
            name: DEFAULT_PROFILE_NAME.to_string(),
            on_session_lock: Some(d.on_session_lock),
            on_session_unlock: Some(d.on_session_unlock),
            on_greeter_wake: Some(d.on_greeter_wake),
            idle_timeout_seconds: Some(d.idle_timeout_seconds),
            on_idle_timeout: Some(d.on_idle_timeout),
            on_idle_resume: Some(d.on_idle_resume),
        }
    }

    /// Materialized-but-empty default body — every field `None`.
    /// Mirror of [`SystemProfileConfig::empty_default`]; used by
    /// [`UserSection::ensure_default`].
    pub fn empty_default() -> Self {
        Self {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..Self::default()
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`. Fields
    /// that are `Some` in `self` take precedence. Names don't merge —
    /// see `SystemProfileConfig::merge_with_base` for the rationale.
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
// Config sections
// =============================================================================

/// System-level configuration section.
///
/// Contains system profiles that define daemon lifecycle behavior.
/// Located under `[system]` in the TOML config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemSection {
    /// System profiles keyed by UUID. Each profile's `name` lives
    /// inside its body; identity is the UUID. The reserved
    /// [`DEFAULT_SYSTEM_PROFILE_UUID`] hosts the `"default"` profile
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

    /// Look up a profile by its friendly name. `None` if no profile
    /// in the catalog carries this name. Names are enforced unique
    /// at create time, so an `Option` is always definitive (no
    /// "first match" ambiguity).
    pub fn find_profile_by_name(&self, name: &str) -> Option<(Uuid, &SystemProfileConfig)> {
        self.profiles
            .iter()
            .find(|(_, body)| body.name == name)
            .map(|(id, body)| (*id, body))
    }

    /// Materialize the reserved-default profile if absent and pin its
    /// name to [`DEFAULT_PROFILE_NAME`]. Invariant: the default UUID
    /// always has a body in memory, even when the on-disk TOML omits
    /// the section. The synthesized body is empty (every field
    /// `None`) so the resolution walk falls through to
    /// [`ResolvedSystemProfile::default`]; the section appears on
    /// disk only after the user makes their first edit.
    pub fn ensure_default(&mut self) {
        self.profiles
            .entry(DEFAULT_SYSTEM_PROFILE_UUID)
            .and_modify(|body| body.name = DEFAULT_PROFILE_NAME.to_string())
            .or_insert_with(SystemProfileConfig::empty_default);
    }
}

/// User-level configuration section.
///
/// Contains user profiles that define session event behavior.
/// Located under `[user]` in the TOML config.
///
/// In the system config, the [`DEFAULT_USER_PROFILE_UUID`] profile is
/// the admin baseline inherited by users who have not created their
/// own config file. In the user config, user-defined profiles
/// override / extend the admin baseline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserSection {
    /// User profiles keyed by UUID. Same shape + invariants as
    /// [`SystemSection::profiles`]; default profile lives at
    /// [`DEFAULT_USER_PROFILE_UUID`].
    #[serde(default)]
    pub profiles: BTreeMap<Uuid, UserProfileConfig>,
}

impl UserSection {
    /// Look up a profile by its friendly name.
    pub fn find_profile_by_name(&self, name: &str) -> Option<(Uuid, &UserProfileConfig)> {
        self.profiles
            .iter()
            .find(|(_, body)| body.name == name)
            .map(|(id, body)| (*id, body))
    }

    /// Resolve a user profile by UUID, walking the inheritance chain:
    /// this profile → user-config default ([`DEFAULT_USER_PROFILE_UUID`])
    /// → admin baseline lookup (by this profile's name on
    /// `admin_user_profiles`, then `admin_user_default`) → hardcoded
    /// baseline.
    ///
    /// The per-profile `UserProfile1` D-Bus interface uses this to
    /// report the resolved value for each typed property. The admin
    /// baseline is name-keyed on the session-service side
    /// (`SessionState::admin_user_profiles`), so we merge by the
    /// profile body's `name` field rather than by UUID at that hop.
    pub fn resolve_profile_by_uuid(
        &self,
        uuid: Uuid,
        admin_user_default: &UserProfileConfig,
        admin_user_profiles: &std::collections::HashMap<String, UserProfileConfig>,
    ) -> ResolvedUserProfile {
        let named = self.profiles.get(&uuid);
        let user_default = if uuid != DEFAULT_USER_PROFILE_UUID {
            self.profiles.get(&DEFAULT_USER_PROFILE_UUID)
        } else {
            None
        };
        // Look up an admin profile by the body's name (the admin
        // baseline is name-keyed; the user catalog tracks its own UUID
        // but routes through name for the cross-layer merge).
        let admin_named = named.and_then(|body| admin_user_profiles.get(&body.name));

        // Merge chain: user profile → user default → admin profile →
        // admin default. Each layer fills `None` fields from the next.
        let merged = match (named, user_default, admin_named) {
            (Some(p), Some(d), Some(a)) => p
                .merge_with_base(d)
                .merge_with_base(a)
                .merge_with_base(admin_user_default),
            (Some(p), Some(d), None) => p.merge_with_base(d).merge_with_base(admin_user_default),
            (Some(p), None, Some(a)) => p.merge_with_base(a).merge_with_base(admin_user_default),
            (Some(p), None, None) => p.merge_with_base(admin_user_default),
            (None, Some(d), _) => d.merge_with_base(admin_user_default),
            (None, None, _) => admin_user_default.clone(),
        };
        merged.resolve()
    }

    /// Materialize the reserved-default profile if absent and pin its
    /// name to [`DEFAULT_PROFILE_NAME`]. See
    /// [`SystemSection::ensure_default`] for the rationale.
    pub fn ensure_default(&mut self) {
        self.profiles
            .entry(DEFAULT_USER_PROFILE_UUID)
            .and_modify(|body| body.name = DEFAULT_PROFILE_NAME.to_string())
            .or_insert_with(UserProfileConfig::empty_default);
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
    /// `profile.system` selects a system profile for daemon lifecycle events.
    /// Defaults to `"default"`.
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
    /// exists in each section. Missing file → bare default config.
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
    /// Uses XDG directories: `~/.config/tisvlay/config.toml`
    pub fn config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "tisvlay").ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            ))
        })?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Resolve the effective profile for a TV.
    ///
    /// Resolution chain:
    /// 1. User's per-TV profile assignment -> user's profile body (by UUID)
    /// 2. User's [`DEFAULT_USER_PROFILE_UUID`] profile body
    /// 3. Admin baseline by the assigned profile's body name
    /// 4. System default profile (passed in)
    /// 5. Hard-coded defaults
    ///
    /// At each level, individual fields merge (user overrides system).
    /// The admin baseline merge is name-keyed (see `UserSection::resolve_profile_by_uuid`).
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
        let toml_str = r#"
[user.profiles."00000000-0000-0000-0000-000000000002"]
name = "default"
on_session_lock = "screen_off"
on_session_unlock = "screen_on"

[user.profiles."a1b2c3d4-0000-0000-0000-000000000001"]
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
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
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
        let (_, gaming) = config.user.find_profile_by_name("gaming").unwrap();
        assert_eq!(gaming.on_session_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));
    }

    #[test]
    fn test_parse_new_system_config() {
        // New-style system config with system and user profile sections
        let toml_str = r#"
[system.profiles."00000000-0000-0000-0000-000000000001"]
name = "default"
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_no_session = "screen_on"

[user.profiles."00000000-0000-0000-0000-000000000002"]
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
[system.profiles."00000000-0000-0000-0000-000000000001"]
name = "default"
on_startup = "power_on"
on_shutdown = "power_off"

[system.profiles."{guest_uuid}"]
name = "guest"
on_startup = "none"
on_shutdown = "screen_off"

[user.profiles."00000000-0000-0000-0000-000000000002"]
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
profile.system = "00000000-0000-0000-0000-000000000001"
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
        config.user.profiles.insert(
            Uuid::new_v4(),
            UserProfileConfig {
                name: "custom".to_string(),
                on_session_lock: Some(SessionAction::PowerOff),
                ..UserProfileConfig::default()
            },
        );
        config.system.profiles.insert(
            Uuid::new_v4(),
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

        assert!(saved_config.user.find_profile_by_name("custom").is_some());
        assert!(saved_config.user.find_profile_by_name("default").is_none());
        assert!(saved_config.system.find_profile_by_name("guest").is_some());

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
    fn ensure_default_materializes_empty_body_when_missing() {
        // The reserved default UUID must always have a body on the
        // bus, even when the TOML omits the section. The body the
        // daemon synthesizes is empty (every field None) so the
        // resolution walk falls through to the hardcoded baseline.
        // The user can't recover any other way — reserved-name
        // validation blocks `CreateSystemProfile("default")` /
        // `CreateUserProfile("default")`.
        let mut system = SystemSection::default();
        assert!(!system.profiles.contains_key(&DEFAULT_SYSTEM_PROFILE_UUID));
        system.ensure_default();
        let body = system
            .profiles
            .get(&DEFAULT_SYSTEM_PROFILE_UUID)
            .expect("default materialized");
        assert_eq!(body.name, DEFAULT_PROFILE_NAME);
        assert!(body.on_startup.is_none());
        assert!(body.on_shutdown.is_none());
        assert!(body.automate_when.is_none());
        // Resolution falls through to the hardcoded baseline.
        let resolved = system.resolve_profile_by_uuid(DEFAULT_SYSTEM_PROFILE_UUID);
        assert_eq!(resolved, ResolvedSystemProfile::default());

        let mut user = UserSection::default();
        assert!(!user.profiles.contains_key(&DEFAULT_USER_PROFILE_UUID));
        user.ensure_default();
        let body = user
            .profiles
            .get(&DEFAULT_USER_PROFILE_UUID)
            .expect("user default materialized");
        assert_eq!(body.name, DEFAULT_PROFILE_NAME);
        assert!(body.on_session_lock.is_none());
        assert!(body.idle_timeout_seconds.is_none());
    }

    #[test]
    fn ensure_default_preserves_existing_body() {
        // If the TOML already defines the default, ensure_default
        // is a no-op on the body — the user's customizations stay.
        let mut system = SystemSection::default();
        system.profiles.insert(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileConfig {
                name: DEFAULT_PROFILE_NAME.to_string(),
                on_startup: Some(SessionAction::ScreenOff),
                ..SystemProfileConfig::default()
            },
        );
        system.ensure_default();
        let body = system.profiles.get(&DEFAULT_SYSTEM_PROFILE_UUID).unwrap();
        assert_eq!(body.on_startup, Some(SessionAction::ScreenOff));
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
                name: DEFAULT_PROFILE_NAME.to_string(),
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
                name: DEFAULT_PROFILE_NAME.to_string(),
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
[system.profiles."00000000-0000-0000-0000-000000000001"]
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
[user.profiles."00000000-0000-0000-0000-000000000002"]
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
                name: DEFAULT_PROFILE_NAME.to_string(),
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
                name: DEFAULT_PROFILE_NAME.to_string(),
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
        let (_, gaming) = deserialized.user.find_profile_by_name("gaming").unwrap();
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
        let toml_str = r#"
[system.profiles."a1b2c3d4-0000-0000-0000-0000000000c1"]
name = "custom"
on_sleep = "none"
on_wake = "screen_on"

[user.profiles."a1b2c3d4-0000-0000-0000-0000000000c2"]
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
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();

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
        let toml_str = r#"
[system.profiles."00000000-0000-0000-0000-000000000001"]
name = "default"
on_startup = "power_on"

[user.profiles."00000000-0000-0000-0000-000000000002"]
name = "default"
on_session_lock = "screen_off"
idle_timeout_seconds = 300

[user.profiles."a1b2c3d4-0000-0000-0000-00000000f000"]
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
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert!(config.user.find_profile_by_name("default").is_some());
        assert!(config.user.find_profile_by_name("foo").is_some());

        let (_, foo) = config.user.find_profile_by_name("foo").unwrap();
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

        // Merge: system profiles as base, user profiles override
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
                    on_session_lock: Some(SessionAction::ScreenOff),
                    idle_timeout_seconds: Some(300),
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

        let mut user_config = UserConfig::default();
        user_config.user.profiles.insert(
            DEFAULT_USER_PROFILE_UUID,
            UserProfileConfig {
                name: DEFAULT_PROFILE_NAME.to_string(),
                idle_timeout_seconds: Some(600), // user overrides
                ..UserProfileConfig::default()
            },
        );
        user_config.user.profiles.insert(
            Uuid::new_v4(),
            UserProfileConfig {
                name: "gaming".to_string(),
                on_session_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );

        // Merge: system first, then user overrides
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
[user.profiles."00000000-0000-0000-0000-000000000002"]
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
        let system_toml = r#"
[system.profiles."00000000-0000-0000-0000-000000000001"]
name = "default"
on_startup = "power_on"

[user.profiles."00000000-0000-0000-0000-000000000002"]
name = "default"
on_session_lock = "screen_off"

[user.profiles."a1b2c3d4-0000-0000-0000-00000000f000"]
name = "foo"
on_session_lock = "none"

[[tv]]
uuid = "22222222-2222-2222-2222-222222222222"
name = "Office"
vendor = "lg"
managed = true

[tv.lg]
ip = "192.168.1.1"
"#;

        let config: UserConfig = toml::from_str(system_toml).unwrap();
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
                name: DEFAULT_PROFILE_NAME.to_string(),
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
}
