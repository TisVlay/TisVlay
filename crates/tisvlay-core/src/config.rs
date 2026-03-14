//! Configuration file parsing and management.
//!
//! # Configuration Format
//!
//! ```toml
//! [daemon]
//! wake_on_boot = true
//! wake_delay_ms = 3000
//!
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
//! vendor = "lg"
//! profile = "gaming"
//! hdmi_input = "HDMI_1"
//! only_when_active = true
//!
//! # Vendor-specific settings (connection params + vendor features)
//! [tv.lg]
//! ip = "192.168.1.100"
//! mac = "AA:BB:CC:DD:EE:FF"
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

    /// Named profiles for session behavior.
    /// Each TV can reference a profile by name. If not specified, "default" is used.
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

/// Default vendor.
fn default_vendor() -> String {
    "lg".to_string()
}

/// Default profile name.
fn default_profile() -> String {
    "default".to_string()
}

/// Configuration for a single TV.
///
/// This struct contains vendor-agnostic identity and behavior settings.
/// Connection parameters (IP, MAC, etc.) are vendor-specific and stored in
/// the vendor config map, parsed by each vendor crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TvConfig {
    /// Unique identifier for this TV.
    pub id: String,

    /// Display name.
    pub name: String,

    /// Vendor/backend to use for this TV (e.g., "lg", "samsung").
    /// Defaults to "lg" (currently the only supported vendor).
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

    /// Vendor-specific configuration.
    ///
    /// This captures vendor-specific fields (e.g., `lg = { ip = "...", client_key = "..." }`).
    /// Use `get_vendor_config::<T>()` to deserialize into a typed struct.
    #[serde(flatten)]
    pub vendor_config: std::collections::HashMap<String, toml::Value>,
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
    ///
    /// If the profiles map is empty, a "default" profile with built-in values
    /// is added before saving. This makes the config file self-documenting
    /// for users who haven't customized their profiles yet.
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // If no profiles defined, add the default profile with built-in values
        // so users can see and customize them
        let config_to_save = if self.profiles.is_empty() {
            let mut config = self.clone();
            config.profiles.insert("default".to_string(), ProfileConfig::default());
            config
        } else {
            self.clone()
        };

        let content =
            toml::to_string_pretty(&config_to_save).map_err(|e| Error::Io(std::io::Error::other(e)))?;
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
    /// 3. Default ProfileConfig values
    pub fn get_profile_for_tv(&self, tv: &TvConfig) -> ProfileConfig {
        // First try the TV's specified profile
        if let Some(profile) = self.profiles.get(&tv.profile) {
            return profile.clone();
        }

        // Then try the "default" profile
        if let Some(default_profile) = self.profiles.get("default") {
            return default_profile.clone();
        }

        // Fall back to default values
        ProfileConfig::default()
    }

    /// Get a profile by name.
    ///
    /// Returns the named profile, or falls back to "default" profile,
    /// or returns default ProfileConfig values.
    pub fn get_profile(&self, name: &str) -> ProfileConfig {
        if let Some(profile) = self.profiles.get(name) {
            return profile.clone();
        }

        if let Some(default_profile) = self.profiles.get("default") {
            return default_profile.clone();
        }

        ProfileConfig::default()
    }
}

impl TvConfig {
    /// Create a new TV configuration.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            vendor: default_vendor(),
            profile: default_profile(),
            hdmi_input: None,
            only_when_active: false,
            vendor_config: std::collections::HashMap::new(),
        }
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

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.tv.is_empty());
        assert!(config.daemon.wake_on_boot);
        assert_eq!(config.daemon.websocket_port, 3001);
    }

    #[test]
    fn test_parse_config() {
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

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.tv.len(), 1);
        assert_eq!(config.tv[0].vendor, "lg");
        assert_eq!(config.tv[0].profile, "gaming");
        assert_eq!(config.tv[0].hdmi_input, Some("HDMI_1".to_string()));
        assert!(config.tv[0].only_when_active);

        // Check vendor-specific config (including ip/mac) via get_vendor_config
        let lg: TestLgConfig = config.tv[0].get_vendor_config().unwrap();
        assert_eq!(lg.ip, "192.168.1.100");
        assert_eq!(lg.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(lg.client_key, Some("abc123".to_string()));
        assert_eq!(lg.default_input, Some("HDMI_1".to_string()));

        // Check profiles
        assert_eq!(config.profiles.len(), 2);
        let gaming = config.profiles.get("gaming").unwrap();
        assert_eq!(gaming.on_screen_lock, SessionAction::None);
        assert_eq!(gaming.idle_timeout_seconds, 0);
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

        // Connection params are in vendor config, not TvConfig
        let extracted: TestLgConfig = tv.get_vendor_config().unwrap();
        assert_eq!(extracted.ip, "192.168.1.1");
        assert_eq!(extracted.mac, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(extracted, lg_config);
    }

    #[test]
    fn test_profile_resolution() {
        let toml = r#"
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

        let config: Config = toml::from_str(toml).unwrap();

        // TV1 uses gaming profile
        let profile1 = config.get_profile_for_tv(&config.tv[0]);
        assert_eq!(profile1.on_screen_lock, SessionAction::None);

        // TV2 uses default profile
        let profile2 = config.get_profile_for_tv(&config.tv[1]);
        assert_eq!(profile2.on_screen_lock, SessionAction::PowerOff);
    }

    #[test]
    fn test_find_tv() {
        let mut config = Config::default();
        config.tv.push(
            TvConfig::new("tv1", "TV 1")
                .with_vendor("lg")
                .with_vendor_config(&TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:01")),
        );
        config.tv.push(
            TvConfig::new("tv2", "TV 2")
                .with_vendor("lg")
                .with_vendor_config(&TestLgConfig::new("192.168.1.2").with_mac("AA:BB:CC:DD:EE:02")),
        );

        assert!(config.find_tv("tv1").is_some());
        assert!(config.find_tv("tv3").is_none());
    }

    #[test]
    fn test_default_profile_config() {
        let profile = ProfileConfig::default();
        assert_eq!(profile.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(profile.on_screen_unlock, SessionAction::ScreenOn);
        assert_eq!(profile.on_greeter_wake, SessionAction::ScreenOn);
        assert_eq!(profile.on_sleep, SessionAction::PowerOff);
        assert_eq!(profile.on_wake, SessionAction::PowerOn);
        assert_eq!(profile.idle_timeout_seconds, 0);
        assert_eq!(profile.on_idle_timeout, SessionAction::ScreenOff);
    }

    #[test]
    fn test_parse_profile_config() {
        let toml = r#"
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
profile = "custom"

[tv.lg]
ip = "192.168.1.1"
mac = "AA:BB:CC:DD:EE:FF"
"#;

        let config: Config = toml::from_str(toml).unwrap();
        let profile = config.get_profile("custom");
        assert_eq!(profile.on_screen_lock, SessionAction::PowerOff);
        assert_eq!(profile.on_screen_unlock, SessionAction::PowerOn);
        assert_eq!(profile.on_greeter_wake, SessionAction::None);
        assert_eq!(profile.on_sleep, SessionAction::None);
        assert_eq!(profile.on_wake, SessionAction::ScreenOn);
        assert_eq!(profile.idle_timeout_seconds, 300);
        assert_eq!(profile.on_idle_timeout, SessionAction::ScreenOff);
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

        // Create a config with no profiles
        let mut config = Config::default();
        config.tv.push(
            TvConfig::new("test", "Test TV")
                .with_vendor("lg")
                .with_vendor_config(&TestLgConfig::new("192.168.1.1").with_mac("AA:BB:CC:DD:EE:FF")),
        );
        assert!(config.profiles.is_empty());

        // Save to a temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("tisvlay_test_config.toml");
        config.save_to(&temp_file).unwrap();

        // Read back and verify default profile was added
        let mut content = String::new();
        std::fs::File::open(&temp_file)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        // Parse the saved config
        let saved_config: Config = toml::from_str(&content).unwrap();

        // Should have the default profile
        assert!(saved_config.profiles.contains_key("default"));
        let default_profile = saved_config.profiles.get("default").unwrap();
        assert_eq!(default_profile.on_screen_lock, SessionAction::ScreenOff);
        assert_eq!(default_profile.on_screen_unlock, SessionAction::ScreenOn);

        // Clean up
        let _ = std::fs::remove_file(temp_file);
    }

    #[test]
    fn test_save_preserves_existing_profiles() {
        use std::io::Read;

        // Create a config with a custom profile
        let mut config = Config::default();
        config.profiles.insert(
            "custom".to_string(),
            ProfileConfig {
                on_screen_lock: SessionAction::PowerOff,
                ..ProfileConfig::default()
            },
        );

        // Save to a temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("tisvlay_test_config2.toml");
        config.save_to(&temp_file).unwrap();

        // Read back and verify
        let mut content = String::new();
        std::fs::File::open(&temp_file)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let saved_config: Config = toml::from_str(&content).unwrap();

        // Should have the custom profile, not a default one added
        assert!(saved_config.profiles.contains_key("custom"));
        assert!(!saved_config.profiles.contains_key("default"));

        // Clean up
        let _ = std::fs::remove_file(temp_file);
    }
}
