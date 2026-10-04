//! [`VendorConfig`]: a TV's vendor-specific config table.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A TV's vendor-specific config: the table only its vendor crate knows how
/// to interpret (the LG address and client key, the CEC device node, …).
/// The daemon stores and merges it without reading the values.
///
/// It can hold a credential, so `Debug` prints the key names only. Serde
/// sees the plain table, so persisted configs keep their shape. Vendors
/// read it through [`parse`](Self::parse) into their typed config.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VendorConfig(toml::Table);

impl VendorConfig {
    /// An empty config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize a vendor's typed config into the table.
    pub fn from_typed<T: Serialize>(config: &T) -> Result<Self, toml::ser::Error> {
        toml::Table::try_from(config).map(Self)
    }

    /// Deserialize the table into a vendor's typed config.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, toml::de::Error> {
        self.0.clone().try_into()
    }

    /// One value.
    pub fn get(&self, key: &str) -> Option<&toml::Value> {
        self.0.get(key)
    }

    /// One value, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut toml::Value> {
        self.0.get_mut(key)
    }

    /// Set a value, returning the one it replaced.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: impl Into<toml::Value>,
    ) -> Option<toml::Value> {
        self.0.insert(key.into(), value.into())
    }

    /// Remove a value, returning it.
    pub fn remove(&mut self, key: &str) -> Option<toml::Value> {
        self.0.remove(key)
    }

    /// The key names, in table order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// The entries, in table order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &toml::Value)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }
}

impl From<toml::Table> for VendorConfig {
    fn from(table: toml::Table) -> Self {
        Self(table)
    }
}

impl fmt::Debug for VendorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VendorConfig")
            .field("keys", &self.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Typed {
        ip: String,
        client_key: Option<String>,
    }

    #[test]
    fn debug_prints_key_names_only() {
        let mut config = VendorConfig::new();
        config.insert("ip", "192.0.2.1");
        config.insert("client_key", "hunter2-client-key");
        for shown in [format!("{config:?}"), format!("{config:#?}")] {
            assert!(shown.contains("client_key"), "{shown}");
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(!shown.contains("192.0.2.1"), "{shown}");
        }
    }

    #[test]
    fn typed_round_trip() {
        let typed = Typed {
            ip: "192.0.2.1".into(),
            client_key: Some("key".into()),
        };
        let config = VendorConfig::from_typed(&typed).unwrap();
        assert_eq!(
            config.get("ip").and_then(toml::Value::as_str),
            Some("192.0.2.1")
        );
        assert_eq!(config.parse::<Typed>().unwrap(), typed);
    }

    #[test]
    fn serde_sees_the_plain_table() {
        let config: VendorConfig = toml::from_str("ip = \"192.0.2.1\"").unwrap();
        assert_eq!(
            toml::to_string(&config).unwrap().trim(),
            "ip = \"192.0.2.1\""
        );
    }
}
