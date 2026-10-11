//! [`Secret`]: a credential value that can't leak through `Debug`.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, Serializer};

/// A credential (the LG client key, a future token or password) whose
/// `Debug` prints `[REDACTED]`, so it can't reach a log through a `?field`
/// or `#[instrument]`. Reading it is an explicit [`expose`](Self::expose).
///
/// Serde sees a plain string, so persisted configs and protocol payloads
/// keep their shape. There's deliberately no `PartialEq`: compare exposed
/// values where a test needs to.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(SecretString);

impl Secret {
    /// Wrap a credential value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// The credential itself, for the one place that sends or stores it.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let secret = Secret::new("hunter2-client-key");
        let rendered = format!("{secret:?}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn serde_sees_a_plain_string() {
        let secret: Secret = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(secret.expose(), "abc");
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"abc\"");
    }
}
