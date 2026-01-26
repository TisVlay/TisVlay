//! Configuration file parsing and management.
//!
//! This module supports both the legacy configuration format and the new
//! multi-vendor profile-based format.
//!
//! # Configuration Format
//!
//! ## Legacy Format (still supported)
//!
//! ```toml
//! [session]
//! on_screen_lock = "screen_off"
//!
//! [[tv]]
//! id = "living-room"
//! name = "Living Room TV"
//! ip = "192.168.1.100"
//! mac = "AA:BB:CC:DD:EE:FF"
//! client_key = "abc123"
//! ```
//!
//! ## New Profile-Based Format
//!
//! ```toml
//! [profiles.default]
//! on_screen_lock = "screen_off"
//! on_screen_unlock = "screen_on"
//!
//! [profiles.gaming]
//! on_screen_lock = "none"
//! idle_timeout_seconds = 0
//!
//! [[tv]]
//! id = "living-room"
//! name = "Living Room TV"
//! ip = "192.168.1.100"
//! mac = "AA:BB:CC:DD:EE:FF"
//! vendor = "lg"
//! profile = "gaming"
//! hdmi_input = "HDMI_1"
//! only_when_active = true
//!
//! [tv.lg]
//! client_key = "abc123"
//! default_input = "HDMI_1"
//! ```

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Main configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// Session service settings (user-specific).
    /// This is the legacy single-profile config. New configs should use `profiles`.
    #[serde(default)]
    pub session: SessionConfig,

    /// Named profiles for session behavior.
    /// Each TV can reference a profile by name. If not specified, "default" is used.
    /// If profiles is empty, the legacy `session` config is used as the default profile.
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,

    /// Configured TVs.
    #[serde(default)]
    pub tv: Vec<TvConfig>,
}

/// Daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Whether to wake TVs on system boot.
    #[serde(default)]
    pub wake_on_boot: bool,

    /// Delay in milliseconds after WoL before sending commands.
    #[serde(default = "default_wake_delay")]
    pub wake_delay_ms: u64,

    /// WebSocket port to use (3000 for ws://, 3001 for wss://).
    #[serde(default = "default_ws_port")]
    pub websocket_port: u16,

    /// Whether to use TLS (wss://).
    #[serde(default = "default_use_tls")]
    pub use_tls: bool,

    /// Connection timeout in milliseconds.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_ms: u64,

    /// Reconnection interval in milliseconds.
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval_ms: u64,
}

fn default_wake_delay() -> u64 {
    3000
}

fn default_ws_port() -> u16 {
    3001
}

fn default_use_tls() -> bool {
    true
}

fn default_connect_timeout() -> u64 {
    5000
}

fn default_reconnect_interval() -> u64 {
    10000
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            wake_on_boot: true,
            wake_delay_ms: default_wake_delay(),
            websocket_port: default_ws_port(),
            use_tls: default_use_tls(),
            connect_timeout_ms: default_connect_timeout(),
            reconnect_interval_ms: default_reconnect_interval(),
        }
    }
}

/// Profile configuration for session behavior.
///
/// A profile defines what actions to take in response to system events.
/// TVs can reference profiles by name, allowing different TVs to have
/// different behaviors (e.g., desk vs gaming profiles).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    /// Action on screen lock event.
    #[serde(default = "default_on_lock")]
    pub on_screen_lock: SessionAction,

    /// Action on screen unlock event.
    #[serde(default = "default_on_unlock")]
    pub on_screen_unlock: SessionAction,

    /// Action when user interacts with the lock screen (before unlocking).
    #[serde(default = "default_on_greeter_wake")]
    pub on_greeter_wake: SessionAction,

    /// Action on system sleep/suspend.
    #[serde(default = "default_on_sleep")]
    pub on_sleep: SessionAction,

    /// Action on system wake/resume.
    #[serde(default = "default_on_wake")]
    pub on_wake: SessionAction,

    /// Idle timeout in seconds (0 to disable).
    #[serde(default)]
    pub idle_timeout_seconds: u64,

    /// Action on idle timeout.
    #[serde(default = "default_on_idle")]
    pub on_idle_timeout: SessionAction,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            on_screen_lock: default_on_lock(),
            on_screen_unlock: default_on_unlock(),
            on_greeter_wake: default_on_greeter_wake(),
            on_sleep: default_on_sleep(),
            on_wake: default_on_wake(),
            idle_timeout_seconds: 0,
            on_idle_timeout: default_on_idle(),
        }
    }
}

impl From<SessionConfig> for ProfileConfig {
    fn from(session: SessionConfig) -> Self {
        Self {
            on_screen_lock: session.on_screen_lock,
            on_screen_unlock: session.on_screen_unlock,
            on_greeter_wake: session.on_greeter_wake,
            on_sleep: session.on_sleep,
            on_wake: session.on_wake,
            idle_timeout_seconds: session.idle_timeout_seconds,
            on_idle_timeout: session.on_idle_timeout,
        }
    }
}

impl From<ProfileConfig> for SessionConfig {
    fn from(profile: ProfileConfig) -> Self {
        Self {
            on_screen_lock: profile.on_screen_lock,
            on_screen_unlock: profile.on_screen_unlock,
            on_greeter_wake: profile.on_greeter_wake,
            on_sleep: profile.on_sleep,
            on_wake: profile.on_wake,
            idle_timeout_seconds: profile.idle_timeout_seconds,
            on_idle_timeout: profile.on_idle_timeout,
        }
    }
}

/// Session service configuration (user-specific).
///
/// This is the legacy single-profile configuration. New configurations
/// should use the `profiles` map with named profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Action on screen lock event.
    #[serde(default = "default_on_lock")]
    pub on_screen_lock: SessionAction,

    /// Action on screen unlock event.
    #[serde(default = "default_on_unlock")]
    pub on_screen_unlock: SessionAction,

    /// Action when user interacts with the lock screen (before unlocking).
    /// This fires when the user moves the mouse or presses a key while locked,
    /// allowing the TV to unblank so the user can see the password prompt.
    ///
    /// Detected via Wayland ext-idle-notify-v1 protocol on supported compositors
    /// (KDE Plasma 6+, Sway, Hyprland, GNOME, etc.).
    #[serde(default = "default_on_greeter_wake")]
    pub on_greeter_wake: SessionAction,

    /// Action on system sleep/suspend.
    #[serde(default = "default_on_sleep")]
    pub on_sleep: SessionAction,

    /// Action on system wake/resume.
    #[serde(default = "default_on_wake")]
    pub on_wake: SessionAction,

    /// Idle timeout in seconds (0 to disable).
    #[serde(default)]
    pub idle_timeout_seconds: u64,

    /// Action on idle timeout.
    #[serde(default = "default_on_idle")]
    pub on_idle_timeout: SessionAction,
}

fn default_on_lock() -> SessionAction {
    SessionAction::ScreenOff
}

fn default_on_unlock() -> SessionAction {
    SessionAction::ScreenOn
}

fn default_on_sleep() -> SessionAction {
    SessionAction::PowerOff
}

fn default_on_wake() -> SessionAction {
    SessionAction::PowerOn
}

fn default_on_idle() -> SessionAction {
    SessionAction::ScreenOff
}

fn default_on_greeter_wake() -> SessionAction {
    SessionAction::ScreenOn
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            on_screen_lock: default_on_lock(),
            on_screen_unlock: default_on_unlock(),
            on_greeter_wake: default_on_greeter_wake(),
            on_sleep: default_on_sleep(),
            on_wake: default_on_wake(),
            idle_timeout_seconds: 0, // Disabled by default
            on_idle_timeout: default_on_idle(),
        }
    }
}

/// Actions the session service can take.
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

/// Default vendor for backwards compatibility.
fn default_vendor() -> String {
    "lg".to_string()
}

/// Default profile name.
fn default_profile() -> String {
    "default".to_string()
}

/// Configuration for a single TV.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvConfig {
    /// Unique identifier for this TV.
    pub id: String,

    /// Display name.
    pub name: String,

    /// IP address.
    pub ip: String,

    /// MAC address for Wake-on-LAN.
    /// Can be empty string if MAC is not known.
    #[serde(default)]
    pub mac: String,

    /// Vendor/backend to use for this TV (e.g., "lg", "samsung").
    /// Defaults to "lg" for backwards compatibility.
    #[serde(default = "default_vendor")]
    pub vendor: String,

    /// Profile name for session behavior.
    /// Defaults to "default".
    #[serde(default = "default_profile")]
    pub profile: String,

    /// HDMI input this PC is connected to.
    /// Used for display-aware control when `only_when_active` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdmi_input: Option<String>,

    /// Only control this TV when it's on the specified HDMI input.
    /// Requires `hdmi_input` to be set.
    #[serde(default)]
    pub only_when_active: bool,

    // =========================================================================
    // Legacy fields (still supported for backwards compatibility)
    // =========================================================================
    /// Client key obtained during pairing (LG WebOS).
    /// Deprecated: Use `[tv.lg] client_key` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,

    /// Default HDMI input to switch to on wake (LG WebOS).
    /// Deprecated: Use `[tv.lg] default_input` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_input: Option<String>,

    // =========================================================================
    // Vendor-specific configuration
    // =========================================================================
    /// LG WebOS specific configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lg: Option<LgVendorConfig>,
    // Future: samsung, sony, roku, etc.
}

/// LG WebOS specific configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LgVendorConfig {
    /// Client key obtained during pairing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,

    /// Default HDMI input to switch to on wake.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_input: Option<String>,
}

impl Config {
    /// Load configuration from the default path.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    /// Load configuration from a specific path.
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Save configuration to the default path.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    /// Save configuration to a specific path.
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content =
            toml::to_string_pretty(self).map_err(|e| Error::Io(std::io::Error::other(e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Get the default configuration file path.
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

    /// Get the system-wide configuration file path.
    ///
    /// Returns `/etc/tisvlay/config.toml`.
    pub fn system_config_path() -> PathBuf {
        PathBuf::from("/etc/tisvlay/config.toml")
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

    /// Get the profile configuration for a TV.
    ///
    /// Resolution order:
    /// 1. Named profile from `profiles` map if TV specifies one
    /// 2. "default" profile from `profiles` map
    /// 3. Legacy `session` config (converted to ProfileConfig)
    pub fn get_profile_for_tv(&self, tv: &TvConfig) -> ProfileConfig {
        // First try the TV's specified profile
        if let Some(profile) = self.profiles.get(&tv.profile) {
            return profile.clone();
        }

        // Then try the "default" profile
        if let Some(default_profile) = self.profiles.get("default") {
            return default_profile.clone();
        }

        // Fall back to legacy session config (always, regardless of whether
        // other profiles exist - the session config serves as the implicit default)
        ProfileConfig::from(self.session.clone())
    }

    /// Get a profile by name.
    ///
    /// Returns the named profile, or falls back to "default" profile,
    /// or creates a ProfileConfig from the legacy session config.
    pub fn get_profile(&self, name: &str) -> ProfileConfig {
        if let Some(profile) = self.profiles.get(name) {
            return profile.clone();
        }

        if let Some(default_profile) = self.profiles.get("default") {
            return default_profile.clone();
        }

        // Fall back to legacy session config
        ProfileConfig::from(self.session.clone())
    }
}

impl TvConfig {
    /// Create a new TV configuration.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        ip: impl Into<String>,
        mac: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ip: ip.into(),
            mac: mac.into(),
            vendor: default_vendor(),
            profile: default_profile(),
            hdmi_input: None,
            only_when_active: false,
            client_key: None,
            default_input: None,
            lg: None,
        }
    }

    /// Create a new TV configuration with a specific vendor.
    pub fn new_with_vendor(
        id: impl Into<String>,
        name: impl Into<String>,
        ip: impl Into<String>,
        mac: impl Into<String>,
        vendor: impl Into<String>,
    ) -> Self {
        Self {
            vendor: vendor.into(),
            ..Self::new(id, name, ip, mac)
        }
    }

    /// Set the client key (legacy method, sets on both legacy field and lg config).
    pub fn with_client_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.client_key = Some(key.clone());
        self.lg = Some(LgVendorConfig {
            client_key: Some(key),
            default_input: self.lg.as_ref().and_then(|lg| lg.default_input.clone()),
        });
        self
    }

    /// Set the default input (legacy method).
    pub fn with_default_input(mut self, input: impl Into<String>) -> Self {
        let input = input.into();
        self.default_input = Some(input.clone());
        self.lg = Some(LgVendorConfig {
            client_key: self.lg.as_ref().and_then(|lg| lg.client_key.clone()),
            default_input: Some(input),
        });
        self
    }

    /// Set the vendor.
    pub fn with_vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = vendor.into();
        self
    }

    /// Set the profile.
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = profile.into();
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

    /// Set LG-specific configuration.
    pub fn with_lg_config(mut self, lg: LgVendorConfig) -> Self {
        self.lg = Some(lg);
        self
    }

    /// Get the effective client key (checks both legacy and vendor-specific fields).
    pub fn effective_client_key(&self) -> Option<&str> {
        // Prefer vendor-specific config
        if let Some(ref lg) = self.lg {
            if let Some(ref key) = lg.client_key {
                return Some(key);
            }
        }
        // Fall back to legacy field
        self.client_key.as_deref()
    }

    /// Get the effective default input (checks both legacy and vendor-specific fields).
    pub fn effective_default_input(&self) -> Option<&str> {
        // Prefer vendor-specific config
        if let Some(ref lg) = self.lg {
            if let Some(ref input) = lg.default_input {
                return Some(input);
            }
        }
        // Fall back to legacy field
        self.default_input.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.tv.is_empty());
        assert!(config.daemon.wake_on_boot);
        assert_eq!(config.daemon.websocket_port, 3001);
    }

    #[test]
    fn test_parse_legacy_config() {
        let toml = r#"
[daemon]
wake_on_boot = true
wake_delay_ms = 5000

[[tv]]
id = "living-room"
name = "Living Room TV"
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123"
default_input = "HDMI_1"
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].id, "living-room");
        assert_eq!(config.tv[0].client_key, Some("abc123".to_string()));
        assert_eq!(config.tv[0].vendor, "lg"); // Default vendor
        assert_eq!(config.tv[0].profile, "default"); // Default profile
    }

    #[test]
    fn test_parse_new_config_format() {
        let toml = r#"
[profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"

[profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

[[tv]]
id = "living-room"
name = "Living Room TV"
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
vendor = "lg"
profile = "gaming"
hdmi_input = "HDMI_1"
only_when_active = true

[tv.lg]
client_key = "abc123"
default_input = "HDMI_1"
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].vendor, "lg");
        assert_eq!(config.tv[0].profile, "gaming");
        assert_eq!(config.tv[0].hdmi_input, Some("HDMI_1".to_string()));
        assert!(config.tv[0].only_when_active);

        // Check vendor-specific config
        let lg = config.tv[0].lg.as_ref().unwrap();
        assert_eq!(lg.client_key, Some("abc123".to_string()));
        assert_eq!(lg.default_input, Some("HDMI_1".to_string()));

        // Check profiles
        assert_eq!(config.profiles.len(), 2);
        let gaming = config.profiles.get("gaming").unwrap();
        assert_eq!(gaming.on_screen_lock, SessionAction::None);
        assert_eq!(gaming.idle_timeout_seconds, 0);
    }

    #[test]
    fn test_effective_client_key() {
        // Legacy format
        let tv = TvConfig::new("test", "Test", "192.168.1.1", "AA:BB:CC:DD:EE:FF")
            .with_client_key("legacy_key");
        assert_eq!(tv.effective_client_key(), Some("legacy_key"));

        // Vendor-specific config takes precedence
        let mut tv = TvConfig::new("test", "Test", "192.168.1.1", "AA:BB:CC:DD:EE:FF");
        tv.client_key = Some("legacy_key".to_string());
        tv.lg = Some(LgVendorConfig {
            client_key: Some("vendor_key".to_string()),
            default_input: None,
        });
        assert_eq!(tv.effective_client_key(), Some("vendor_key"));
    }

    #[test]
    fn test_profile_resolution() {
        let toml = r#"
[session]
on_screen_lock = "power_off"

[profiles.gaming]
on_screen_lock = "none"

[[tv]]
id = "tv1"
name = "TV 1"
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:01"
profile = "gaming"

[[tv]]
id = "tv2"
name = "TV 2"
ip = "192.168.1.2"
mac = "AA:BB:CC:DD:EE:02"
"#;

        let config: Config = toml::from_str(toml).unwrap();

        // TV1 uses gaming profile
        let profile1 = config.get_profile_for_tv(&config.tv[0]);
        assert_eq!(profile1.on_screen_lock, SessionAction::None);

        // TV2 uses default (falls back to session config since no "default" profile)
        let profile2 = config.get_profile_for_tv(&config.tv[1]);
        assert_eq!(profile2.on_screen_lock, SessionAction::PowerOff);
    }

    #[test]
    fn test_find_tv() {
        let mut config = Config::default();
        config.tv.push(TvConfig::new(
            "tv1",
            "TV 1",
            "192.168.1.1",
            "AA:BB:CC:DD:EE:01",
        ));
        config.tv.push(TvConfig::new(
            "tv2",
            "TV 2",
            "192.168.1.2",
            "AA:BB:CC:DD:EE:02",
        ));

        assert!(config.find_tv("tv1").is_some());
        assert!(config.find_tv("tv3").is_none());
    }

    #[test]
    fn test_default_session_config() {
        let config = SessionConfig::default();
        assert_eq!(config.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(config.on_screen_unlock, SessionAction::ScreenOn);
        assert_eq!(config.on_greeter_wake, SessionAction::ScreenOn);
        assert_eq!(config.on_sleep, SessionAction::PowerOff);
        assert_eq!(config.on_wake, SessionAction::PowerOn);
        assert_eq!(config.idle_timeout_seconds, 0);
        assert_eq!(config.on_idle_timeout, SessionAction::ScreenOff);
    }

    #[test]
    fn test_parse_session_config() {
        let toml = r#"
[session]
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
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.session.on_screen_lock, SessionAction::PowerOff);
        assert_eq!(config.session.on_screen_unlock, SessionAction::PowerOn);
        assert_eq!(config.session.on_greeter_wake, SessionAction::None);
        assert_eq!(config.session.on_sleep, SessionAction::None);
        assert_eq!(config.session.on_wake, SessionAction::ScreenOn);
        assert_eq!(config.session.idle_timeout_seconds, 300);
        assert_eq!(config.session.on_idle_timeout, SessionAction::ScreenOff);
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
    fn test_profile_to_session_conversion() {
        let profile = ProfileConfig {
            on_screen_lock: SessionAction::PowerOff,
            on_screen_unlock: SessionAction::PowerOn,
            on_greeter_wake: SessionAction::None,
            on_sleep: SessionAction::None,
            on_wake: SessionAction::ScreenOn,
            idle_timeout_seconds: 300,
            on_idle_timeout: SessionAction::ScreenOff,
        };

        let session: SessionConfig = profile.clone().into();
        assert_eq!(session.on_screen_lock, SessionAction::PowerOff);
        assert_eq!(session.idle_timeout_seconds, 300);

        let back: ProfileConfig = session.into();
        assert_eq!(back.on_screen_lock, profile.on_screen_lock);
    }
}
