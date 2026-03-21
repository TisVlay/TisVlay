//! Configuration file parsing and management.
//!
//! # Configuration Split
//!
//! The configuration is split between system and user levels:
//!
//! ## System Config (`/var/lib/tisvlay/config.toml`)
//!
//! Owned by `tisvlayd`. Contains TV definitions, credentials, daemon settings,
//! system power policy, and a default profile inherited by users.
//!
//! ```toml
//! [daemon]
//! wake_on_boot = true
//! wake_delay_ms = 3000
//! on_shutdown = "power_off"
//! on_no_active_session = "none"
//!
//! [profiles.default]
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
//! [profiles.default]
//! idle_timeout_seconds = 600
//!
//! [profiles.gaming]
//! on_screen_lock = "none"
//! idle_timeout_seconds = 0
//!
//! [tv.living-room]
//! profile = "gaming"
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
// ProfileConfig (with Option<T> fields for merge support)
// =============================================================================

/// Profile configuration for session behavior.
///
/// Fields are `Option<T>` to support layered inheritance:
/// user profile -> system default profile -> hard-coded defaults.
///
/// Use [`ProfileConfig::resolve()`] or [`ProfileConfig::merge_with_base()`]
/// to produce a [`ResolvedProfile`] with concrete values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileConfig {
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
}

impl ProfileConfig {
    /// Create a profile with all fields set to the built-in sensible defaults.
    ///
    /// Use this when saving a system config to make profiles self-documenting,
    /// or as the system default profile.
    pub fn full_default() -> Self {
        let d = ResolvedProfile::default();
        Self {
            on_screen_lock: Some(d.on_screen_lock),
            on_screen_unlock: Some(d.on_screen_unlock),
            on_greeter_wake: Some(d.on_greeter_wake),
            on_sleep: Some(d.on_sleep),
            on_wake: Some(d.on_wake),
            idle_timeout_seconds: Some(d.idle_timeout_seconds),
            on_idle_timeout: Some(d.on_idle_timeout),
        }
    }

    /// Merge this profile with a base profile.
    ///
    /// Fields that are `None` in `self` are filled from `base`.
    /// Fields that are `Some` in `self` take precedence.
    pub fn merge_with_base(&self, base: &ProfileConfig) -> ProfileConfig {
        ProfileConfig {
            on_screen_lock: self.on_screen_lock.or(base.on_screen_lock),
            on_screen_unlock: self.on_screen_unlock.or(base.on_screen_unlock),
            on_greeter_wake: self.on_greeter_wake.or(base.on_greeter_wake),
            on_sleep: self.on_sleep.or(base.on_sleep),
            on_wake: self.on_wake.or(base.on_wake),
            idle_timeout_seconds: self.idle_timeout_seconds.or(base.idle_timeout_seconds),
            on_idle_timeout: self.on_idle_timeout.or(base.on_idle_timeout),
        }
    }

    /// Resolve this profile to concrete values.
    ///
    /// Any `None` fields are filled with hard-coded defaults from
    /// [`ResolvedProfile::default()`].
    pub fn resolve(&self) -> ResolvedProfile {
        let d = ResolvedProfile::default();
        ResolvedProfile {
            on_screen_lock: self.on_screen_lock.unwrap_or(d.on_screen_lock),
            on_screen_unlock: self.on_screen_unlock.unwrap_or(d.on_screen_unlock),
            on_greeter_wake: self.on_greeter_wake.unwrap_or(d.on_greeter_wake),
            on_sleep: self.on_sleep.unwrap_or(d.on_sleep),
            on_wake: self.on_wake.unwrap_or(d.on_wake),
            idle_timeout_seconds: self.idle_timeout_seconds.unwrap_or(d.idle_timeout_seconds),
            on_idle_timeout: self.on_idle_timeout.unwrap_or(d.on_idle_timeout),
        }
    }
}

// =============================================================================
// ResolvedProfile (fully resolved, no Option fields)
// =============================================================================

/// A fully resolved profile with no optional fields.
///
/// This is the result of merging a [`ProfileConfig`] chain
/// (user profile -> system default -> hard-coded defaults).
/// Consumers (session monitor, idle detector) use this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
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
}

impl Default for ResolvedProfile {
    fn default() -> Self {
        Self {
            on_screen_lock: SessionAction::ScreenOff,
            on_screen_unlock: SessionAction::ScreenOn,
            on_greeter_wake: SessionAction::ScreenOn,
            on_sleep: SessionAction::PowerOff,
            on_wake: SessionAction::PowerOn,
            idle_timeout_seconds: 0,
            on_idle_timeout: SessionAction::ScreenOff,
        }
    }
}

// =============================================================================
// DaemonConfig
// =============================================================================

/// Daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Whether to wake TVs on system boot.
    #[serde(default)]
    pub wake_on_boot: bool,

    /// Delay in milliseconds after WoL before sending commands.
    #[serde(default = "default_wake_delay")]
    pub wake_delay_ms: u64,

    /// Action when system shuts down or reboots.
    #[serde(default = "default_on_shutdown")]
    pub on_shutdown: SessionAction,

    /// Action when all user sessions end (e.g., after logout, before login).
    #[serde(default)]
    pub on_no_active_session: SessionAction,
}

fn default_wake_delay() -> u64 {
    3000
}

fn default_on_shutdown() -> SessionAction {
    SessionAction::PowerOff
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            wake_on_boot: true,
            wake_delay_ms: default_wake_delay(),
            on_shutdown: default_on_shutdown(),
            on_no_active_session: SessionAction::None,
        }
    }
}

// =============================================================================
// TvConfig
// =============================================================================

/// Default vendor.
fn default_vendor() -> String {
    "lg".to_string()
}

/// Configuration for a single TV.
///
/// This struct contains vendor-agnostic identity and hardware settings.
/// Connection parameters (IP, MAC, etc.) are vendor-specific and stored in
/// the vendor config map, parsed by each vendor crate.
///
/// Profile assignment has moved to [`UserTvConfig`] in the user config.
/// The `profile` field is retained for backward compatibility with existing
/// config files but is deprecated and not serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvConfig {
    /// Unique identifier for this TV.
    pub id: String,

    /// Display name.
    pub name: String,

    /// Vendor/backend to use for this TV (e.g., "lg", "samsung").
    #[serde(default = "default_vendor")]
    pub vendor: String,

    /// Deprecated: profile assignment has moved to user config.
    /// Retained for backward compatibility with existing config files.
    #[serde(default, skip_serializing)]
    pub profile: Option<String>,

    /// HDMI input this PC is connected to.
    /// Used for display-aware control when `only_when_active` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdmi_input: Option<String>,

    /// Only control this TV when it's on the specified HDMI input.
    /// Requires `hdmi_input` to be set.
    #[serde(default)]
    pub only_when_active: bool,

    /// Vendor-specific configuration.
    ///
    /// This captures vendor-specific fields (e.g., `lg = { ip = "...", client_key = "..." }`).
    /// Use `get_vendor_config::<T>()` to deserialize into a typed struct.
    #[serde(flatten)]
    pub vendor_config: HashMap<String, toml::Value>,
}

impl TvConfig {
    /// Create a new TV configuration.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            vendor: default_vendor(),
            profile: None,
            hdmi_input: None,
            only_when_active: false,
            vendor_config: HashMap::new(),
        }
    }

    /// Set the vendor.
    pub fn with_vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = vendor.into();
        self
    }

    /// Set the profile (deprecated — use UserConfig for profile assignment).
    #[deprecated(note = "Profile assignment has moved to UserConfig. This is for backward compat.")]
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
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
/// Contains TV definitions, vendor credentials, daemon settings, system power
/// policy, and a default profile inherited by users who have no user config.
///
/// This is also aliased as [`Config`] for backward compatibility.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemConfig {
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// Named profiles.
    ///
    /// In the system config, `[profiles.default]` is the system default profile
    /// inherited by users who have not created their own config file.
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,

    /// Configured TVs.
    #[serde(default)]
    pub tv: Vec<TvConfig>,
}

/// Backward-compatible alias for [`SystemConfig`].
pub type Config = SystemConfig;

impl SystemConfig {
    /// Load configuration from the default user path.
    ///
    /// Uses XDG directories: `~/.config/tisvlay/config.toml`
    pub fn load() -> Result<Self> {
        let path = Self::user_config_path()?;
        Self::load_from(&path)
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

    /// Save configuration to the default user path.
    pub fn save(&self) -> Result<()> {
        let path = Self::user_config_path()?;
        self.save_to(&path)
    }

    /// Save configuration to a specific path.
    ///
    /// If the profiles map is empty, a "default" profile with built-in values
    /// is added before saving. This makes the config file self-documenting
    /// for users who haven't customized their profiles yet.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // If no profiles defined, add the default profile with built-in values
        // so users can see and customize them
        let config_to_save = if self.profiles.is_empty() {
            let mut config = self.clone();
            config
                .profiles
                .insert("default".to_string(), ProfileConfig::full_default());
            config
        } else {
            self.clone()
        };

        let content = toml::to_string_pretty(&config_to_save)
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Get the default user configuration file path.
    ///
    /// Uses XDG directories: `~/.config/tisvlay/config.toml`
    pub fn user_config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "tisvlay").ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            ))
        })?;

        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Backward-compatible alias for [`user_config_path`](Self::user_config_path).
    pub fn config_path() -> Result<PathBuf> {
        Self::user_config_path()
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

    /// Get the first configured TV (default).
    pub fn default_tv(&self) -> Option<&TvConfig> {
        self.tv.first()
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

    /// Get the system default profile.
    ///
    /// Returns the `[profiles.default]` section if present, otherwise an empty
    /// [`ProfileConfig`] (all `None` fields, meaning hard-coded defaults apply).
    pub fn get_default_profile(&self) -> &ProfileConfig {
        static EMPTY: std::sync::OnceLock<ProfileConfig> = std::sync::OnceLock::new();
        self.profiles
            .get("default")
            .unwrap_or_else(|| EMPTY.get_or_init(ProfileConfig::default))
    }

    /// Get the system default profile, resolved to concrete values.
    pub fn get_default_profile_resolved(&self) -> ResolvedProfile {
        self.get_default_profile().resolve()
    }

    /// Get the profile configuration for a TV.
    ///
    /// This uses the deprecated `TvConfig.profile` field for backward
    /// compatibility with single-config-file setups. New code should use
    /// [`UserConfig::resolve_profile()`] instead.
    ///
    /// Resolution order:
    /// 1. Named profile from `profiles` map if TV specifies one (deprecated)
    /// 2. "default" profile from `profiles` map
    /// 3. Hard-coded defaults via `ProfileConfig::full_default()`
    pub fn get_profile_for_tv(&self, tv: &TvConfig) -> ProfileConfig {
        // First try the TV's specified profile (backward compat)
        if let Some(ref profile_name) = tv.profile {
            if let Some(profile) = self.profiles.get(profile_name) {
                return profile.clone();
            }
        }

        // Then try the "default" profile
        if let Some(default_profile) = self.profiles.get("default") {
            return default_profile.clone();
        }

        // Fall back to full default values
        ProfileConfig::full_default()
    }

    /// Get a profile by name.
    ///
    /// Returns the named profile, or falls back to "default" profile,
    /// or returns `ProfileConfig::full_default()`.
    pub fn get_profile(&self, name: &str) -> ProfileConfig {
        if let Some(profile) = self.profiles.get(name) {
            return profile.clone();
        }

        if let Some(default_profile) = self.profiles.get("default") {
            return default_profile.clone();
        }

        ProfileConfig::full_default()
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
    /// The name "default" is reserved: it applies to any TV without an
    /// explicit profile assignment. Fields not specified inherit from the
    /// system default profile, NOT from hard-coded defaults.
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,

    /// Per-TV profile assignments. Key is TV ID from system config.
    #[serde(default)]
    pub tv: HashMap<String, UserTvConfig>,
}

/// User's per-TV settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTvConfig {
    /// Profile name to use for this TV.
    #[serde(default = "default_profile_name")]
    pub profile: String,
}

fn default_profile_name() -> String {
    "default".to_string()
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

        let content = toml::to_string_pretty(self)
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Get the user config file path.
    ///
    /// Uses XDG directories: `~/.config/tisvlay/config.toml`
    pub fn config_path() -> Result<PathBuf> {
        SystemConfig::user_config_path()
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
    pub fn resolve_profile(
        &self,
        tv_id: &str,
        system_default: &ProfileConfig,
    ) -> ResolvedProfile {
        // 1. Find which profile name applies to this TV
        let profile_name = self
            .tv
            .get(tv_id)
            .map(|t| t.profile.as_str())
            .unwrap_or("default");

        // 2. Get the user's profile definition (may be partial)
        let user_profile = self.profiles.get(profile_name);

        // 3. Get the user's default profile (for field inheritance within user config)
        let user_default = if profile_name != "default" {
            self.profiles.get("default")
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
            (None, _) => {
                // No user profile found — use system default directly
                system_default.clone()
            }
        };

        // 5. Resolve any remaining Nones to hard-coded defaults
        merged.resolve()
    }

    /// Get the profile name assigned to a TV.
    ///
    /// Returns `"default"` if no explicit assignment exists.
    pub fn get_tv_profile_name(&self, tv_id: &str) -> &str {
        self.tv
            .get(tv_id)
            .map(|t| t.profile.as_str())
            .unwrap_or("default")
    }

    /// Assign a profile to a TV.
    pub fn set_tv_profile(&mut self, tv_id: impl Into<String>, profile: impl Into<String>) {
        let profile = profile.into();
        self.tv.insert(
            tv_id.into(),
            UserTvConfig { profile },
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
    // SystemConfig (backward compat) tests
    // =========================================================================

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.tv.is_empty());
        assert!(config.daemon.wake_on_boot);
        assert_eq!(config.daemon.on_shutdown, SessionAction::PowerOff);
        assert_eq!(config.daemon.on_no_active_session, SessionAction::None);
    }

    #[test]
    fn test_parse_config_backward_compat() {
        // Old-style config with profile on [[tv]] — should still parse
        let toml_str = r#"
[profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"

[profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

[[tv]]
id = "living-room"
name = "Living Room TV"
vendor = "lg"
profile = "gaming"
hdmi_input = "HDMI_1"
only_when_active = true

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123"
default_input = "HDMI_1"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].vendor, "lg");
        // Deprecated profile field still readable for backward compat
        assert_eq!(config.tv[0].profile, Some("gaming".to_string()));
        assert_eq!(config.tv[0].hdmi_input, Some("HDMI_1".to_string()));
        assert!(config.tv[0].only_when_active);

        // Check vendor-specific config
        let lg: TestLgConfig = config.tv[0].get_vendor_config().unwrap();
        assert_eq!(lg.ip, "192.168.1.100");
        assert_eq!(lg.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(lg.client_key, Some("abc123".to_string()));
        assert_eq!(lg.default_input, Some("HDMI_1".to_string()));

        // Check profiles (now Option fields)
        assert_eq!(config.profiles.len(), 2);
        let gaming = config.profiles.get("gaming").unwrap();
        assert_eq!(gaming.on_screen_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));

        // Backward compat: get_profile_for_tv still works with deprecated profile field
        let profile = config.get_profile_for_tv(&config.tv[0]);
        assert_eq!(profile.on_screen_lock, Some(SessionAction::None));
    }

    #[test]
    fn test_parse_new_system_config() {
        // New-style system config without profile on [[tv]]
        let toml_str = r#"
[daemon]
wake_on_boot = true
on_shutdown = "power_off"
on_no_active_session = "screen_on"

[profiles.default]
on_screen_lock = "screen_off"
idle_timeout_seconds = 300

[[tv]]
id = "living-room"
name = "Living Room G4"
vendor = "lg"
hdmi_input = "HDMI_1"

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: SystemConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.daemon.on_shutdown, SessionAction::PowerOff);
        assert_eq!(config.daemon.on_no_active_session, SessionAction::ScreenOn);
        assert_eq!(config.tv[0].profile, None);

        let default_profile = config.get_default_profile();
        assert_eq!(default_profile.on_screen_lock, Some(SessionAction::ScreenOff));
        assert_eq!(default_profile.idle_timeout_seconds, Some(300));
        // Unspecified field
        assert_eq!(default_profile.on_greeter_wake, None);
    }

    #[test]
    fn test_vendor_config() {
        let lg_config = TestLgConfig {
            ip: "192.168.1.1".to_string(),
            mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
            client_key: Some("test_key".to_string()),
            default_input: Some("HDMI_1".to_string()),
        };

        let tv = TvConfig::new("test", "Test")
            .with_vendor("lg")
            .with_vendor_config(&lg_config);

        assert_eq!(tv.vendor, "lg");

        let extracted: TestLgConfig = tv.get_vendor_config().unwrap();
        assert_eq!(extracted.ip, "192.168.1.1");
        assert_eq!(extracted.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(extracted, lg_config);
    }

    #[test]
    fn test_find_tv() {
        let mut config = Config::default();
        config.tv.push(
            TvConfig::new("tv1", "TV 1")
                .with_vendor("lg")
                .with_vendor_config(
                    &TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:01"),
                ),
        );
        config.tv.push(
            TvConfig::new("tv2", "TV 2")
                .with_vendor("lg")
                .with_vendor_config(
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
    fn test_save_adds_default_profile_when_empty() {
        use std::io::Read;

        let mut config = Config::default();
        config.tv.push(
            TvConfig::new("test", "Test TV")
                .with_vendor("lg")
                .with_vendor_config(
                    &TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:FF"),
                ),
        );
        assert!(config.profiles.is_empty());

        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("tisvlay_test_config_4c.toml");
        config.save_to(&temp_file).unwrap();

        let mut content = String::new();
        std::fs::File::open(&temp_file)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let saved_config: Config = toml::from_str(&content).unwrap();

        assert!(saved_config.profiles.contains_key("default"));
        let default_profile = saved_config.profiles.get("default").unwrap();
        // full_default() sets all fields to Some(...)
        assert_eq!(
            default_profile.on_screen_lock,
            Some(SessionAction::ScreenOff)
        );
        assert_eq!(
            default_profile.on_screen_unlock,
            Some(SessionAction::ScreenOn)
        );

        let _ = std::fs::remove_file(temp_file);
    }

    #[test]
    fn test_save_preserves_existing_profiles() {
        use std::io::Read;

        let mut config = Config::default();
        config.profiles.insert(
            "custom".to_string(),
            ProfileConfig {
                on_screen_lock: Some(SessionAction::PowerOff),
                ..ProfileConfig::default()
            },
        );

        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("tisvlay_test_config2_4c.toml");
        config.save_to(&temp_file).unwrap();

        let mut content = String::new();
        std::fs::File::open(&temp_file)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let saved_config: Config = toml::from_str(&content).unwrap();

        assert!(saved_config.profiles.contains_key("custom"));
        assert!(!saved_config.profiles.contains_key("default"));

        let _ = std::fs::remove_file(temp_file);
    }

    // =========================================================================
    // ResolvedProfile tests
    // =========================================================================

    #[test]
    fn test_resolved_profile_default() {
        let resolved = ResolvedProfile::default();
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
        let full = ProfileConfig::full_default();
        let resolved = full.resolve();
        assert_eq!(resolved, ResolvedProfile::default());
    }

    // =========================================================================
    // Profile merge/resolve tests
    // =========================================================================

    #[test]
    fn test_merge_with_base_partial_override() {
        let base = ProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..ProfileConfig::default()
        };

        let override_profile = ProfileConfig {
            on_screen_lock: Some(SessionAction::None),
            // idle_timeout_seconds not specified — should inherit from base
            ..ProfileConfig::default()
        };

        let merged = override_profile.merge_with_base(&base);
        assert_eq!(merged.on_screen_lock, Some(SessionAction::None));
        assert_eq!(merged.idle_timeout_seconds, Some(300)); // inherited
        assert_eq!(merged.on_sleep, None); // neither specified
    }

    #[test]
    fn test_merge_with_base_empty_override() {
        let base = ProfileConfig::full_default();
        let empty = ProfileConfig::default(); // all None

        let merged = empty.merge_with_base(&base);
        // All fields should come from base
        assert_eq!(merged.on_screen_lock, base.on_screen_lock);
        assert_eq!(merged.idle_timeout_seconds, base.idle_timeout_seconds);
    }

    #[test]
    fn test_merge_with_base_full_override() {
        let base = ProfileConfig::full_default();
        let full_override = ProfileConfig {
            on_screen_lock: Some(SessionAction::None),
            on_screen_unlock: Some(SessionAction::None),
            on_greeter_wake: Some(SessionAction::None),
            on_sleep: Some(SessionAction::None),
            on_wake: Some(SessionAction::None),
            idle_timeout_seconds: Some(999),
            on_idle_timeout: Some(SessionAction::PowerOff),
        };

        let merged = full_override.merge_with_base(&base);
        assert_eq!(merged.on_screen_lock, Some(SessionAction::None));
        assert_eq!(merged.idle_timeout_seconds, Some(999));
        assert_eq!(merged.on_idle_timeout, Some(SessionAction::PowerOff));
    }

    #[test]
    fn test_resolve_fills_nones() {
        let partial = ProfileConfig {
            on_screen_lock: Some(SessionAction::None),
            // Everything else is None
            ..ProfileConfig::default()
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
[profiles.default]
idle_timeout_seconds = 600

[profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

[tv.living-room]
profile = "gaming"
"#;

        let config: UserConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.profiles.len(), 2);
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv.get("living-room").unwrap().profile, "gaming");
    }

    #[test]
    fn test_user_config_resolve_profile_with_assignment() {
        let system_default = ProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..ProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.profiles.insert(
            "gaming".to_string(),
            ProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                // idle_timeout_seconds not specified — should inherit from system
                ..ProfileConfig::default()
            },
        );
        user.set_tv_profile("living-room", "gaming");

        let resolved = user.resolve_profile("living-room", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::None); // user gaming
        assert_eq!(resolved.idle_timeout_seconds, 300); // inherited from system
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff); // hard-coded default
    }

    #[test]
    fn test_user_config_resolve_profile_no_assignment() {
        let system_default = ProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..ProfileConfig::default()
        };

        let user = UserConfig::default(); // no profiles, no TV assignments

        // TV without assignment uses system default
        let resolved = user.resolve_profile("office", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(resolved.idle_timeout_seconds, 300);
    }

    #[test]
    fn test_user_config_resolve_profile_user_default_overrides_system() {
        let system_default = ProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(300),
            ..ProfileConfig::default()
        };

        let mut user = UserConfig::default();
        user.profiles.insert(
            "default".to_string(),
            ProfileConfig {
                idle_timeout_seconds: Some(600), // user overrides system
                ..ProfileConfig::default()
            },
        );

        // TV without explicit assignment uses user's default profile
        let resolved = user.resolve_profile("office", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::ScreenOff); // from system
        assert_eq!(resolved.idle_timeout_seconds, 600); // user override
    }

    #[test]
    fn test_user_config_resolve_profile_named_inherits_user_default() {
        let system_default = ProfileConfig {
            on_screen_lock: Some(SessionAction::ScreenOff),
            idle_timeout_seconds: Some(100),
            on_sleep: Some(SessionAction::PowerOff),
            ..ProfileConfig::default()
        };

        let mut user = UserConfig::default();
        // User's default profile overrides system idle timeout
        user.profiles.insert(
            "default".to_string(),
            ProfileConfig {
                idle_timeout_seconds: Some(600),
                ..ProfileConfig::default()
            },
        );
        // User's gaming profile overrides screen lock only
        user.profiles.insert(
            "gaming".to_string(),
            ProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                ..ProfileConfig::default()
            },
        );
        user.set_tv_profile("living-room", "gaming");

        let resolved = user.resolve_profile("living-room", &system_default);
        assert_eq!(resolved.on_screen_lock, SessionAction::None); // gaming
        assert_eq!(resolved.idle_timeout_seconds, 600); // user default (not system 100)
        assert_eq!(resolved.on_sleep, SessionAction::PowerOff); // system default
    }

    #[test]
    fn test_user_config_get_tv_profile_name() {
        let mut user = UserConfig::default();
        user.set_tv_profile("living-room", "gaming");

        assert_eq!(user.get_tv_profile_name("living-room"), "gaming");
        assert_eq!(user.get_tv_profile_name("office"), "default");
    }

    #[test]
    fn test_user_config_roundtrip() {
        let mut user = UserConfig::default();
        user.profiles.insert(
            "gaming".to_string(),
            ProfileConfig {
                on_screen_lock: Some(SessionAction::None),
                idle_timeout_seconds: Some(0),
                ..ProfileConfig::default()
            },
        );
        user.set_tv_profile("living-room", "gaming");

        let serialized = toml::to_string_pretty(&user).unwrap();
        let deserialized: UserConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.profiles.len(), 1);
        let gaming = deserialized.profiles.get("gaming").unwrap();
        assert_eq!(gaming.on_screen_lock, Some(SessionAction::None));
        assert_eq!(gaming.idle_timeout_seconds, Some(0));
        // None fields should not be serialized
        assert_eq!(gaming.on_sleep, None);
        assert_eq!(
            deserialized.tv.get("living-room").unwrap().profile,
            "gaming"
        );
    }

    // =========================================================================
    // Backward compatibility with old single-config profile resolution
    // =========================================================================

    #[test]
    fn test_backward_compat_profile_resolution() {
        // Old-style config with profile = "gaming" on [[tv]]
        let toml_str = r#"
[profiles.default]
on_screen_lock = "power_off"

[profiles.gaming]
on_screen_lock = "none"

[[tv]]
id = "tv1"
name = "TV 1"
vendor = "lg"
profile = "gaming"

[tv.lg]
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:01"

[[tv]]
id = "tv2"
name = "TV 2"
vendor = "lg"

[tv.lg]
ip = "192.168.1.2"
mac = "AA:BB:CC:DD:EE:02"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();

        // TV1 uses gaming profile (via deprecated profile field)
        let profile1 = config.get_profile_for_tv(&config.tv[0]);
        assert_eq!(profile1.on_screen_lock, Some(SessionAction::None));

        // TV2 uses default profile
        let profile2 = config.get_profile_for_tv(&config.tv[1]);
        assert_eq!(profile2.on_screen_lock, Some(SessionAction::PowerOff));
    }

    #[test]
    fn test_parse_profile_config_all_fields() {
        let toml_str = r#"
[profiles.custom]
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

[tv.lg]
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let profile = config.get_profile("custom");
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
}
