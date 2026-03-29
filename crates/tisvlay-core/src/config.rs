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
//! [system.profiles.default]
//! on_startup = "power_on"
//! on_shutdown = "power_off"
//! on_reboot = "none"
//! on_no_session = "none"
//!
//! [user.profiles.default]
//! on_screen_lock = "screen_off"
//! on_screen_unlock = "screen_on"
//!
//! [[tv]]
//! id = "living-room"
//! name = "Living Room TV"
//! vendor = "lg"
//! hdmi_input = "HDMI_1"
//! only_when_active = true
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
//! [user.profiles.default]
//! idle_timeout_seconds = 600
//!
//! [user.profiles.gaming]
//! on_screen_lock = "none"
//! idle_timeout_seconds = 0
//!
//! [tv.living-room]
//! profile.user = "gaming"
//! ```

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
}

impl SystemProfileConfig {
    /// Create a profile with all fields set to the built-in sensible defaults.
    ///
    /// Use this when saving a system config to make profiles self-documenting.
    pub fn full_default() -> Self {
        let d = ResolvedSystemProfile::default();
        Self {
            on_startup: Some(d.on_startup),
            on_shutdown: Some(d.on_shutdown),
            on_reboot: Some(d.on_reboot),
            on_no_session: Some(d.on_no_session),
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`.
    /// Fields that are `Some` in `self` take precedence.
    pub fn merge_with_base(&self, base: &SystemProfileConfig) -> SystemProfileConfig {
        SystemProfileConfig {
            on_startup: self.on_startup.or(base.on_startup),
            on_shutdown: self.on_shutdown.or(base.on_shutdown),
            on_reboot: self.on_reboot.or(base.on_reboot),
            on_no_session: self.on_no_session.or(base.on_no_session),
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
}

impl Default for ResolvedSystemProfile {
    fn default() -> Self {
        Self {
            on_startup: SessionAction::PowerOn,
            on_shutdown: SessionAction::PowerOff,
            on_reboot: SessionAction::None,
            on_no_session: SessionAction::None,
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
    /// Action on screen lock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_screen_lock: Option<SessionAction>,

    /// Action on screen unlock event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_screen_unlock: Option<SessionAction>,

    /// Action when user interacts with the lock screen (before unlocking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_greeter_wake: Option<SessionAction>,

    /// Action on system sleep/suspend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_sleep: Option<SessionAction>,

    /// Action on system wake/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wake: Option<SessionAction>,

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
            on_screen_lock: Some(d.on_screen_lock),
            on_screen_unlock: Some(d.on_screen_unlock),
            on_greeter_wake: Some(d.on_greeter_wake),
            on_sleep: Some(d.on_sleep),
            on_wake: Some(d.on_wake),
            idle_timeout_seconds: Some(d.idle_timeout_seconds),
            on_idle_timeout: Some(d.on_idle_timeout),
            on_idle_resume: Some(d.on_idle_resume),
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`.
    /// Fields that are `Some` in `self` take precedence.
    pub fn merge_with_base(&self, base: &UserProfileConfig) -> UserProfileConfig {
        UserProfileConfig {
            on_screen_lock: self.on_screen_lock.or(base.on_screen_lock),
            on_screen_unlock: self.on_screen_unlock.or(base.on_screen_unlock),
            on_greeter_wake: self.on_greeter_wake.or(base.on_greeter_wake),
            on_sleep: self.on_sleep.or(base.on_sleep),
            on_wake: self.on_wake.or(base.on_wake),
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
            on_screen_lock: self.on_screen_lock.unwrap_or(d.on_screen_lock),
            on_screen_unlock: self.on_screen_unlock.unwrap_or(d.on_screen_unlock),
            on_greeter_wake: self.on_greeter_wake.unwrap_or(d.on_greeter_wake),
            on_sleep: self.on_sleep.unwrap_or(d.on_sleep),
            on_wake: self.on_wake.unwrap_or(d.on_wake),
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
    /// Action on screen lock event.
    pub on_screen_lock: SessionAction,
    /// Action on screen unlock event.
    pub on_screen_unlock: SessionAction,
    /// Action when user interacts with the lock screen.
    pub on_greeter_wake: SessionAction,
    /// Action on system sleep/suspend.
    pub on_sleep: SessionAction,
    /// Action on system wake/resume.
    pub on_wake: SessionAction,
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
            on_screen_lock: SessionAction::ScreenOff,
            on_screen_unlock: SessionAction::ScreenOn,
            on_greeter_wake: SessionAction::ScreenOn,
            on_sleep: SessionAction::PowerOff,
            on_wake: SessionAction::PowerOn,
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
    /// Named system profiles for daemon lifecycle events.
    ///
    /// `[system.profiles.default]` is the baseline. Per-TV system profile
    /// assignments can reference other named profiles (e.g., "guest").
    #[serde(default)]
    pub profiles: HashMap<String, SystemProfileConfig>,
}

/// User-level configuration section.
///
/// Contains user profiles that define session event behavior.
/// Located under `[user]` in the TOML config.
///
/// In the system config, `[user.profiles.default]` is the admin baseline
/// inherited by users who have not created their own config file.
/// In the user config, user-defined profiles override/extend the admin baseline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserSection {
    /// Named user profiles for session events.
    #[serde(default)]
    pub profiles: HashMap<String, UserProfileConfig>,
}

// =============================================================================
// TvProfileAssignment
// =============================================================================

/// Per-TV system profile assignment.
///
/// Stored on `TvConfig` (in the system config's `[[tv]]` entries).
/// Determines which system profile governs this TV's daemon lifecycle behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvProfileAssignment {
    /// Name of the system profile to use. Defaults to "default".
    #[serde(default = "default_profile_name")]
    pub system: String,
}

impl Default for TvProfileAssignment {
    fn default() -> Self {
        Self {
            system: default_profile_name(),
        }
    }
}

/// Per-TV user profile assignment.
///
/// Stored on `UserTvConfig` (in the user config's `[tv.<id>]` entries).
/// Determines which user profile governs this TV's session behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTvProfileAssignment {
    /// Name of the user profile to use. Defaults to "default".
    #[serde(default = "default_profile_name")]
    pub user: String,
}

impl Default for UserTvProfileAssignment {
    fn default() -> Self {
        Self {
            user: default_profile_name(),
        }
    }
}

fn default_profile_name() -> String {
    "default".to_string()
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
///
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvConfig {
    /// Unique identifier for this TV.
    pub id: String,

    /// Display name.
    pub name: String,

    /// Vendor/backend to use for this TV (e.g., "lg", "samsung").
    pub vendor: String,

    /// HDMI input this PC is connected to.
    /// Used for display-aware control when `only_when_active` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdmi_input: Option<String>,

    /// Only control this TV when it's on the specified HDMI input.
    /// Requires `hdmi_input` to be set.
    pub only_when_active: bool,

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
    /// Create a new TV configuration.
    ///
    /// `managed` controls whether this TV participates in automation (boot WoL,
    /// session events, shutdown). Pass `true` for normal TVs; `false` to register
    /// the TV for manual control only. There is no default — callers must be explicit.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        vendor: impl Into<String>,
        managed: bool,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            vendor: vendor.into(),
            hdmi_input: None,
            only_when_active: false,
            managed,
            profile: TvProfileAssignment::default(),
            vendor_config: HashMap::new(),
        }
    }

    /// Set the HDMI input for display-aware control.
    pub fn with_hdmi_input(mut self, input: impl Into<String>) -> Self {
        self.hdmi_input = Some(input.into());
        self
    }

    /// Set whether to only control when active.
    pub fn with_only_when_active(mut self, only_when_active: bool) -> Self {
        self.only_when_active = only_when_active;
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
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let config: SystemConfig = toml::from_str(&content)?;
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

    /// Find a TV by ID.
    pub fn find_tv(&self, id: &str) -> Option<&TvConfig> {
        self.tv.iter().find(|tv| tv.id == id)
    }

    /// Find a TV by ID (mutable).
    pub fn find_tv_mut(&mut self, id: &str) -> Option<&mut TvConfig> {
        self.tv.iter_mut().find(|tv| tv.id == id)
    }

    /// Add or update a TV configuration.
    pub fn upsert_tv(&mut self, tv: TvConfig) {
        if let Some(existing) = self.find_tv_mut(&tv.id) {
            *existing = tv;
        } else {
            self.tv.push(tv);
        }
    }

    /// Remove a TV by ID.
    pub fn remove_tv(&mut self, id: &str) -> bool {
        let len = self.tv.len();
        self.tv.retain(|tv| tv.id != id);
        self.tv.len() < len
    }

    /// Resolve the effective system profile for a TV.
    ///
    /// Resolution chain:
    /// 1. TV's assigned system profile (by name)
    /// 2. "default" system profile
    /// 3. Hard-coded defaults
    pub fn resolve_system_profile(&self, tv: &TvConfig) -> ResolvedSystemProfile {
        let name = &tv.profile.system;
        let named = self.system.profiles.get(name);
        let default = if name != "default" {
            self.system.profiles.get("default")
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

    /// Per-TV profile assignments. Key is TV ID from system config.
    #[serde(default)]
    pub tv: HashMap<String, UserTvConfig>,
}

/// User's per-TV settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTvConfig {
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
    /// 1. User's per-TV profile assignment -> user's named profile
    /// 2. User's "default" profile
    /// 3. System default profile (passed in)
    /// 4. Hard-coded defaults
    ///
    /// At each level, individual fields merge (user overrides system).
    pub fn resolve_user_profile(&self, tv_id: &str, system_default: &UserProfileConfig) -> ResolvedUserProfile {
        // 1. Find which profile name applies to this TV
        let profile_name = self
            .tv
            .get(tv_id)
            .map(|t| t.profile.user.as_str())
            .unwrap_or("default");

        // 2. Get the user's profile definition (may be partial)
        let user_profile = self.user.profiles.get(profile_name);

        // 3. Get the user's default profile (for field inheritance within user config)
        let user_default = if profile_name != "default" {
            self.user.profiles.get("default")
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
                // Profile name not found in user config — fall through to user's default
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

    /// Get the profile name assigned to a TV.
    ///
    /// Returns `"default"` if no explicit assignment exists.
    pub fn get_tv_user_profile_name(&self, tv_id: &str) -> &str {
        self.tv
            .get(tv_id)
            .map(|t| t.profile.user.as_str())
            .unwrap_or("default")
    }

    /// Assign a profile to a TV.
    pub fn set_tv_user_profile(&mut self, tv_id: impl Into<String>, profile: impl Into<String>) {
        let profile = profile.into();
        self.tv.insert(
            tv_id.into(),
            UserTvConfig {
                profile: UserTvProfileAssignment { user: profile },
            },
        );
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
        let tv = TvConfig::new("test", "Test", "lg", true);
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
[user.profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"

[user.profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

[[tv]]
id = "living-room"
name = "Living Room TV"
vendor = "lg"
hdmi_input = "HDMI_1"
only_when_active = true
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
        assert_eq!(config.tv[0].hdmi_input, Some("HDMI_1".to_string()));
        assert!(config.tv[0].only_when_active);

        // Check vendor-specific config
        let lg: TestLgConfig = config.tv[0].get_vendor_config().unwrap();
        assert_eq!(lg.ip, "192.168.1.100");
        assert_eq!(lg.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(lg.client_key, Some("abc123".to_string()));
        assert_eq!(lg.default_input, Some("HDMI_1".to_string()));

        // Check user profiles
        assert_eq!(config.user.profiles.len(), 2);
        let gaming = config.user.profiles.get("gaming").unwrap();
        assert_eq!(gaming.on_screen_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));
    }

    #[test]
    fn test_parse_new_system_config() {
        // New-style system config with system and user profile sections
        let toml_str = r#"
[system.profiles.default]
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_no_session = "screen_on"

[user.profiles.default]
on_screen_lock = "screen_off"
idle_timeout_seconds = 300

[[tv]]
id = "living-room"
name = "Living Room G4"
vendor = "lg"
hdmi_input = "HDMI_1"
only_when_active = false
managed = true

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();

        // Check system profile
        let sys_default = config.system.profiles.get("default").unwrap();
        assert_eq!(sys_default.on_startup, Some(SessionAction::PowerOn));
        assert_eq!(sys_default.on_shutdown, Some(SessionAction::PowerOff));
        assert_eq!(sys_default.on_reboot, Some(SessionAction::None));
        assert_eq!(sys_default.on_no_session, Some(SessionAction::ScreenOn));

        // Check user profile
        let user_default = config.user.profiles.get("default").unwrap();
        assert_eq!(
            user_default.on_screen_lock,
            Some(SessionAction::ScreenOff)
        );
        assert_eq!(user_default.idle_timeout_seconds, Some(300));
        // Unspecified field
        assert_eq!(user_default.on_greeter_wake, None);
    }

    #[test]
    fn test_vendor_config() {
        let lg_config = TestLgConfig {
            ip: "192.168.1.1".to_string(),
            mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
            client_key: Some("test_key".to_string()),
            default_input: Some("HDMI_1".to_string()),
        };

        let tv = TvConfig::new("test", "Test", "lg", true).with_vendor_config(&lg_config);

        assert_eq!(tv.vendor, "lg");

        let extracted: TestLgConfig = tv.get_vendor_config().unwrap();
        assert_eq!(extracted.ip, "192.168.1.1");
        assert_eq!(extracted.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(extracted, lg_config);
    }

    #[test]
    fn test_find_tv() {
        let mut config = SystemConfig::default();
        config.tv.push(
            TvConfig::new("tv1", "TV 1", "lg", true).with_vendor_config(
                &TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:01"),
            ),
        );
        config.tv.push(
            TvConfig::new("tv2", "TV 2", "lg", true).with_vendor_config(
                &TestLgConfig::new("192.168.1.2").with_mac("AA:BB:CC:DD:EE:02"),
            ),
        );

        assert!(config.find_tv("tv1").is_some());
        assert!(config.find_tv("tv3").is_none());
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
            TvConfig::new("test", "Test TV", "lg", true).with_vendor_config(
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
            "custom".to_string(),
            UserProfileConfig {
                on_screen_lock: Some(SessionAction::PowerOff),
                ..UserProfileConfig::default()
            },
        );
        config.system.profiles.insert(
            "guest".to_string(),
            SystemProfileConfig {
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

        assert!(saved_config.user.profiles.contains_key("custom"));
        assert!(!saved_config.user.profiles.contains_key("default"));
        assert!(saved_config.system.profiles.contains_key("guest"));

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
        config.system.profiles.insert(
            "default".to_string(),
            SystemProfileConfig {
                on_startup: Some(SessionAction::PowerOn),
                on_shutdown: Some(SessionAction::PowerOff),
                ..SystemProfileConfig::default()
            },
        );
        config.system.profiles.insert(
            "guest".to_string(),
            SystemProfileConfig {
                on_startup: Some(SessionAction::None),
                on_shutdown: Some(SessionAction::ScreenOff),
                ..SystemProfileConfig::default()
            },
        );

        // TV with default profile
        let tv_default = TvConfig::new("living-room", "Living Room", "lg", true);
        let resolved = config.resolve_system_profile(&tv_default);
        assert_eq!(resolved.on_startup, SessionAction::PowerOn);
        assert_eq!(resolved.on_shutdown, SessionAction::PowerOff);

        // TV with guest profile
        let mut tv_guest = TvConfig::new("guest-room", "Guest Room", "lg", true);
        tv_guest.profile.system = "guest".to_string();
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
            "default".to_string(),
            SystemProfileConfig {
                on_startup: Some(SessionAction::PowerOn),
                ..SystemProfileConfig::default()
            },
        );

        let mut tv = TvConfig::new("test", "Test", "lg", true);
        tv.profile.system = "nonexistent".to_string();
        let resolved = config.resolve_system_profile(&tv);
        assert_eq!(resolved.on_startup, SessionAction::PowerOn); // from default
    }

    #[test]
    fn test_tv_profile_assignment_serialization() {
        let toml_str = r#"
[system.profiles.default]
on_startup = "power_on"

[[tv]]
id = "living-room"
name = "Living Room"
vendor = "lg"
only_when_active = false
managed = true
profile.system = "guest"

[tv.lg]
ip = "192.168.1.100"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tv[0].profile.system, "guest");
    }

    #[test]
    fn test_tv_profile_assignment_default() {
        let toml_str = r#"
[[tv]]
id = "test"
name = "Test"
vendor = "lg"
only_when_active = false
managed = true

[tv.lg]
ip = "192.168.1.1"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tv[0].profile.system, "default");
    }

    // =========================================================================
    // ResolvedUserProfile tests
    // =========================================================================

    #[test]
    fn test_resolved_profile_default() {
        let resolved = ResolvedUserProfile::default();
        assert_eq!(resolved.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.on_screen_unlock, SessionAction::ScreenOn);
        assert_eq!(resolved.on_greeter_wake, SessionAction::ScreenOn);
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff);
        assert_eq!(resolved.on_wake, SessionAction::PowerOn);
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
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let override_profile = UserProfileConfig {
            on_screen_lock: Some(SessionAction::None),
            // idle_timeout_seconds not specified — should inherit from base
            ..UserProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_screen_lock, Some(SessionAction::None));
        assert_eq!(merged.idle_timeout_seconds, Some(300)); // inherited
        assert_eq!(merged.on_sleep, None); // neither specified
    }

    #[test]
    fn test_merge_with_base_empty_override() {
        let base = UserProfileConfig::full_default();
        let empty = UserProfileConfig::default(); // all None

        let merged = empty.merge_with_base(&base);
        // All fields should come from base
        assert_eq!(merged.on_screen_lock, base.on_screen_lock);
        assert_eq!(merged.idle_timeout_seconds, base.idle_timeout_seconds);
    }

    #[test]
    fn test_merge_with_base_full_override() {
        let base = UserProfileConfig::full_default();
        let full_override = UserProfileConfig {
            on_screen_lock: Some(SessionAction::None),
            on_screen_unlock: Some(SessionAction::None),
            on_greeter_wake: Some(SessionAction::None),
            on_sleep: Some(SessionAction::None),
            on_wake: Some(SessionAction::None),
            idle_timeout_seconds: Some(999),
            on_idle_timeout: Some(SessionAction::PowerOff),
            on_idle_resume: Some(SessionAction::None),
        };

        let merged = full_override.merge_with_base(&base);
        assert_eq!(merged.on_screen_lock, Some(SessionAction::None));
        assert_eq!(merged.idle_timeout_seconds, Some(999));
        assert_eq!(merged.on_idle_timeout, Some(SessionAction::PowerOff));
    }

    #[test]
    fn test_resolve_fills_nones() {
        let partial = UserProfileConfig {
            on_screen_lock: Some(SessionAction::None),
            // Everything else is None
            ..UserProfileConfig::default()
        };

        let resolved = partial.resolve();
        assert_eq!(resolved.on_screen_lock, SessionAction::None); // from partial
        assert_eq!(resolved.on_screen_unlock, SessionAction::ScreenOn); // hard-coded default
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff); // hard-coded default
        assert_eq!(resolved.idle_timeout_seconds, 0); // hard-coded default
    }

    // =========================================================================
    // UserConfig tests
    // =========================================================================

    #[test]
    fn test_parse_user_config() {
        let toml_str = r#"
[user.profiles.default]
idle_timeout_seconds = 600

[user.profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

[tv.living-room]
profile.user = "gaming"
"#;

        let config: UserConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert_eq!(config.tv.len(), 1);
        assert_eq!(
            config.tv.get("living-room").unwrap().profile.user,
            "gaming"
        );
    }

    #[test]
    fn test_user_config_resolve_user_profile_with_assignment() {
        let system_default = UserProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            "gaming".to_string(),
            UserProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                // idle_timeout_seconds not specified — should inherit from system
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living-room", "gaming");

        let resolved = user.resolve_user_profile("living-room", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::None); // user gaming
        assert_eq!(resolved.idle_timeout_seconds, 300); // inherited from system
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff); // hard-coded default
    }

    #[test]
    fn test_user_config_resolve_user_profile_no_assignment() {
        let system_default = UserProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let user = UserConfig::default(); // no profiles, no TV assignments

        // TV without assignment uses system default
        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.idle_timeout_seconds, 300);
    }

    #[test]
    fn test_user_config_resolve_user_profile_user_default_overrides_system() {
        let system_default = UserProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            "default".to_string(),
            UserProfileConfig {
                idle_timeout_seconds: Some(600), // user overrides system
                ..UserProfileConfig::default()
            },
        );

        // TV without explicit assignment uses user's default profile
        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::ScreenOff); // from system
        assert_eq!(resolved.idle_timeout_seconds, 600); // user override
    }

    #[test]
    fn test_user_config_resolve_user_profile_named_inherits_user_default() {
        let system_default = UserProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(100),
            on_sleep: Some(SessionAction::PowerOff),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        // User's default profile overrides system idle timeout
        user.user.profiles.insert(
            "default".to_string(),
            UserProfileConfig {
                idle_timeout_seconds: Some(600),
                ..UserProfileConfig::default()
            },
        );
        // User's gaming profile overrides screen lock only
        user.user.profiles.insert(
            "gaming".to_string(),
            UserProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living-room", "gaming");

        let resolved = user.resolve_user_profile("living-room", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::None); // gaming
        assert_eq!(resolved.idle_timeout_seconds, 600); // user default (not system 100)
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff); // system default
    }

    #[test]
    fn test_user_config_get_tv_user_profile_name() {
        let mut user = UserConfig::default();
        user.set_tv_user_profile("living-room", "gaming");

        assert_eq!(user.get_tv_user_profile_name("living-room"), "gaming");
        assert_eq!(user.get_tv_user_profile_name("office"), "default");
    }

    #[test]
    fn test_user_config_roundtrip() {
        let mut user = UserConfig::default();
        user.user.profiles.insert(
            "gaming".to_string(),
            UserProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                idle_timeout_seconds: Some(0),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living-room", "gaming");

        let serialized = toml::to_string_pretty(&user).unwrap();
        let deserialized: UserConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.user.profiles.len(), 1);
        let gaming = deserialized.user.profiles.get("gaming").unwrap();
        assert_eq!(gaming.on_screen_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));
        // None fields should not be serialized
        assert_eq!(gaming.on_sleep, None);
        assert_eq!(
            deserialized.tv.get("living-room").unwrap().profile.user,
            "gaming"
        );
    }

    #[test]
    fn test_parse_profile_config_all_fields() {
        let toml_str = r#"
[user.profiles.custom]
on_screen_lock = "power_off"
on_screen_unlock = "power_on"
on_greeter_wake = "none"
on_sleep = "none"
on_wake = "screen_on"
idle_timeout_seconds = 300
on_idle_timeout = "screen_off"

[[tv]]
id = "test"
name = "Test"
vendor = "lg"
only_when_active = false
managed = true

[tv.lg]
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        let profile = config
            .user
            .profiles
            .get("custom")
            .cloned()
            .unwrap_or_else(UserProfileConfig::full_default);
        assert_eq!(profile.on_screen_lock, Some(SessionAction::PowerOff));
        assert_eq!(profile.on_screen_unlock, Some(SessionAction::PowerOn));
        assert_eq!(profile.on_greeter_wake, Some(SessionAction::None));
        assert_eq!(profile.on_sleep, Some(SessionAction::None));
        assert_eq!(profile.on_wake, Some(SessionAction::ScreenOn));
        assert_eq!(profile.idle_timeout_seconds, Some(300));
        assert_eq!(profile.on_idle_timeout, Some(SessionAction::ScreenOff));

        // Resolve to concrete values
        let resolved = profile.resolve();
        assert_eq!(resolved.on_screen_lock, SessionAction::PowerOff);
        assert_eq!(resolved.idle_timeout_seconds, 300);
    }

    // =========================================================================
    // System + User profile merging (simulates session service behavior)
    // =========================================================================

    #[test]
    fn test_system_profiles_parsed_from_system_config() {
        // System config has multiple user profiles — session service should see all of them
        let toml_str = r#"
[system.profiles.default]
on_startup = "power_on"

[user.profiles.default]
on_screen_lock = "screen_off"
idle_timeout_seconds = 300

[user.profiles.foo]
on_screen_lock = "none"
idle_timeout_seconds = 0

[[tv]]
id = "office"
name = "Office"
vendor = "lg"
only_when_active = false
managed = true

[tv.lg]
ip = "192.168.1.1"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert!(config.user.profiles.contains_key("default"));
        assert!(config.user.profiles.contains_key("foo"));

        let foo = config.user.profiles.get("foo").unwrap();
        assert_eq!(foo.on_screen_lock, Some(SessionAction::None));
        assert_eq!(foo.idle_timeout_seconds, Some(0));
    }

    #[test]
    fn test_merged_profile_list_system_only() {
        // When user has no config, all profiles come from system config
        let system_profiles: HashMap<String, UserProfileConfig> = [
            (
                "default".to_string(),
                UserProfileConfig {
                    on_screen_lock: Some(SessionAction::ScreenOff),
                    ..UserProfileConfig::default()
                },
            ),
            (
                "foo".to_string(),
                UserProfileConfig {
                    on_screen_lock: Some(SessionAction::None),
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
        for (name, profile) in &user_config.user.profiles {
            merged.insert(name.clone(), profile);
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
                    on_screen_lock: Some(SessionAction::ScreenOff),
                    idle_timeout_seconds: Some(300),
                    ..UserProfileConfig::default()
                },
            ),
            (
                "foo".to_string(),
                UserProfileConfig {
                    on_screen_lock: Some(SessionAction::None),
                    ..UserProfileConfig::default()
                },
            ),
        ]
        .into_iter()
        .collect();

        let mut user_config = UserConfig::default();
        user_config.user.profiles.insert(
            "default".to_string(),
            UserProfileConfig {
                idle_timeout_seconds: Some(600), // user overrides
                ..UserProfileConfig::default()
            },
        );
        user_config.user.profiles.insert(
            "gaming".to_string(),
            UserProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );

        // Merge: system first, then user overrides
        let mut merged: HashMap<String, &UserProfileConfig> = HashMap::new();
        for (name, profile) in &system_profiles {
            merged.insert(name.clone(), profile);
        }
        for (name, profile) in &user_config.user.profiles {
            merged.insert(name.clone(), profile);
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
        assert_eq!(foo.on_screen_lock, Some(SessionAction::None));
    }

    #[test]
    fn test_user_config_ignores_unknown_toml_fields() {
        // UserConfig should parse even if the TOML has extra fields
        let user_only_toml = r#"
[user.profiles.default]
idle_timeout_seconds = 600

[user.profiles.gaming]
on_screen_lock = "none"

[tv.living-room]
profile.user = "gaming"
"#;

        let config: UserConfig = toml::from_str(user_only_toml).unwrap();
        assert_eq!(config.user.profiles.len(), 2);
        assert!(config.user.profiles.contains_key("default"));
        assert!(config.user.profiles.contains_key("gaming"));
        assert_eq!(
            config.tv.get("living-room").unwrap().profile.user,
            "gaming"
        );
    }

    #[test]
    fn test_system_config_with_multiple_profiles_as_user_config_fails() {
        // A system config with [[tv]] (array of tables) cannot be parsed as UserConfig
        // because UserConfig.tv is HashMap (expects [tv.id] table syntax).
        let system_toml = r#"
[system.profiles.default]
on_startup = "power_on"

[user.profiles.default]
on_screen_lock = "screen_off"

[user.profiles.foo]
on_screen_lock = "none"

[[tv]]
id = "office"
name = "Office"
vendor = "lg"

[tv.lg]
ip = "192.168.1.1"
"#;

        let result = toml::from_str::<UserConfig>(system_toml);
        // This should fail because [[tv]] is an array, but UserConfig.tv expects a table
        assert!(
            result.is_err(),
            "System config should not parse as UserConfig"
        );
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

    /// Regression test: when a TV's assigned profile name does not exist
    /// in the user config, resolution must fall through to the user's `[user.profiles.default]`
    /// before reaching the system default.
    #[test]
    fn test_user_config_resolve_user_profile_missing_profile_falls_through_user_default() {
        let system_default = UserProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            "default".to_string(),
            UserProfileConfig {
                idle_timeout_seconds: Some(600),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("living-room", "nonexistent");

        let resolved = user.resolve_user_profile("living-room", &system_default);
        assert_eq!(resolved.idle_timeout_seconds, 600);
        assert_eq!(resolved.on_screen_lock, SessionAction::ScreenOff);
    }

    #[test]
    fn test_resolve_user_profile_with_system_profiles_as_base() {
        let system_default = UserProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            on_screen_unlock: Some(SessionAction::ScreenOn),
            idle_timeout_seconds: Some(300),
            ..UserProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.user.profiles.insert(
            "gaming".to_string(),
            UserProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                ..UserProfileConfig::default()
            },
        );
        user.set_tv_user_profile("office", "gaming");

        let resolved = user.resolve_user_profile("office", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::None);
        assert_eq!(resolved.idle_timeout_seconds, 300);
        assert_eq!(resolved.on_screen_unlock, SessionAction::ScreenOn);
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff);

        let resolved_unknown = user.resolve_user_profile("other-tv", &system_default);
        assert_eq!(resolved_unknown.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(resolved_unknown.idle_timeout_seconds, 300);
    }
}
