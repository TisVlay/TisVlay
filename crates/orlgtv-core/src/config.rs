//! Configuration file parsing and management.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonConfig,

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
    pub mac: String,

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

        let content = toml::to_string_pretty(self)
            .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Get the default configuration file path.
    ///
    /// Uses XDG directories: `~/.config/orlgtv/config.toml`
    pub fn config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "orlgtv")
            .ok_or_else(|| Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            )))?;

        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Get the system-wide configuration file path.
    ///
    /// Returns `/etc/orlgtv/config.toml`.
    pub fn system_config_path() -> PathBuf {
        PathBuf::from("/etc/orlgtv/config.toml")
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
}

impl TvConfig {
    /// Create a new TV configuration.
    pub fn new(id: impl Into<String>, name: impl Into<String>, ip: impl Into<String>, mac: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ip: ip.into(),
            mac: mac.into(),
            client_key: None,
            default_input: None,
        }
    }

    /// Set the client key.
    pub fn with_client_key(mut self, key: impl Into<String>) -> Self {
        self.client_key = Some(key.into());
        self
    }

    /// Set the default input.
    pub fn with_default_input(mut self, input: impl Into<String>) -> Self {
        self.default_input = Some(input.into());
        self
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
    fn test_parse_config() {
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
    }

    #[test]
    fn test_find_tv() {
        let mut config = Config::default();
        config.tv.push(TvConfig::new("tv1", "TV 1", "192.168.1.1", "AA:BB:CC:DD:EE:01"));
        config.tv.push(TvConfig::new("tv2", "TV 2", "192.168.1.2", "AA:BB:CC:DD:EE:02"));

        assert!(config.find_tv("tv1").is_some());
        assert!(config.find_tv("tv3").is_none());
    }
}
