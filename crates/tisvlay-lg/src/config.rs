//! LG WebOS specific configuration.

use serde::{Deserialize, Serialize};

/// LG WebOS specific configuration.
///
/// This contains settings specific to LG WebOS TVs that are stored
/// in the `[tv.lg]` section of the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LgConfig {
    /// Client key obtained during pairing.
    ///
    /// This is a unique key that the TV generates when you accept the
    /// pairing prompt. It's used for subsequent connections without
    /// requiring user interaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,

    /// Default HDMI input to switch to on wake.
    ///
    /// When specified, the TV will switch to this input after being
    /// powered on via Wake-on-LAN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_input: Option<String>,
}

impl LgConfig {
    /// Create a new empty config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a config with a client key.
    pub fn with_client_key(client_key: impl Into<String>) -> Self {
        Self {
            client_key: Some(client_key.into()),
            default_input: None,
        }
    }

    /// Set the client key.
    pub fn set_client_key(&mut self, key: impl Into<String>) {
        self.client_key = Some(key.into());
    }

    /// Set the default input.
    pub fn set_default_input(&mut self, input: impl Into<String>) {
        self.default_input = Some(input.into());
    }
}
