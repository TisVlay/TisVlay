//! Error types for tisvlay-core.
//!
//! This module provides vendor-agnostic error types that can be used across
//! all TV backends. Backend-specific errors can be wrapped in [`Error::Backend`].

use crate::Capabilities;
use std::io;

/// Result type alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur when communicating with TVs.
///
/// This enum is designed to be vendor-agnostic, with backend-specific errors
/// wrapped in the [`Backend`](Error::Backend) variant.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Connection failed (boxed to keep Error small).
    #[error("Connection failed: {0}")]
    Connection(Box<dyn std::error::Error + Send + Sync>),

    /// I/O error (network, file, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Configuration file parsing error.
    #[error("Config error: {0}")]
    Config(#[from] toml::de::Error),

    /// Configuration validation error (valid TOML but invalid semantics).
    #[error("Config validation error: {0}")]
    ConfigValidation(String),

    /// TV rejected the registration/pairing request.
    #[error("TV rejected pairing request")]
    PairingRejected,

    /// TV pairing timed out (user didn't respond to prompt).
    #[error("Pairing timed out - no response from TV")]
    PairingTimeout,

    /// Not registered with the TV (no credentials).
    #[error("Not registered with TV - pairing required")]
    NotRegistered,

    /// TV returned an error response.
    #[error("TV error: {0}")]
    TvError(String),

    /// Request timed out waiting for response.
    #[error("Request timed out")]
    RequestTimeout,

    /// Connection was closed unexpectedly.
    #[error("Connection closed")]
    ConnectionClosed,

    /// Invalid TV response format.
    #[error("Invalid response from TV: {0}")]
    InvalidResponse(String),

    /// Wake-on-LAN failed.
    #[error("Wake-on-LAN failed: {0}")]
    WakeOnLan(String),

    /// Invalid MAC address format.
    #[error("Invalid MAC address: {0}")]
    InvalidMacAddress(String),

    /// TV not found in configuration.
    #[error("TV not found: {0}")]
    TvNotFound(String),

    /// No TVs configured.
    #[error("No TVs configured")]
    NoTvsConfigured,

    /// SSDP discovery failed.
    #[error("Discovery failed: {0}")]
    Discovery(String),

    // =========================================================================
    // Multi-vendor errors
    // =========================================================================
    /// Operation not supported by this TV/backend.
    ///
    /// Includes the capability that was required but not available.
    #[error("Operation not supported: {message} (requires capability: {capability})")]
    NotSupported {
        /// Human-readable description of the unsupported operation.
        message: String,
        /// The capability that was required.
        capability: String,
    },

    /// Unknown vendor/backend.
    #[error("Unknown vendor: {0}")]
    UnknownVendor(String),

    /// Backend-specific error.
    ///
    /// Wraps vendor-specific errors that don't fit into the common error types.
    #[error("Backend error ({vendor}): {message}")]
    Backend {
        /// Vendor identifier (e.g., "lg", "samsung").
        vendor: String,
        /// Error message.
        message: String,
        /// Optional underlying error (boxed for object safety).
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Profile not found.
    #[error("Profile not found: {0}")]
    ProfileNotFound(String),

    /// Pairing is already in progress.
    #[error("Pairing already in progress for TV: {0}")]
    PairingInProgress(String),

    /// Pairing was cancelled.
    #[error("Pairing was cancelled")]
    PairingCancelled,

    /// Invalid input provided during pairing.
    #[error("Invalid pairing input: {0}")]
    InvalidPairingInput(String),
}

impl Error {
    /// Create a "not supported" error for a missing capability.
    pub fn not_supported(message: impl Into<String>, capability: Capabilities) -> Self {
        let cap_name = capability
            .to_strings()
            .into_iter()
            .next()
            .unwrap_or("unknown");

        Self::NotSupported {
            message: message.into(),
            capability: cap_name.to_string(),
        }
    }

    /// Create a backend-specific error.
    pub fn backend(vendor: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Backend {
            vendor: vendor.into(),
            message: message.into(),
            source: None,
        }
    }

    /// Create a backend-specific error with a source error.
    pub fn backend_with_source<E>(
        vendor: impl Into<String>,
        message: impl Into<String>,
        source: E,
    ) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend {
            vendor: vendor.into(),
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Check if this error indicates the TV is not connected.
    pub fn is_not_connected(&self) -> bool {
        matches!(
            self,
            Self::ConnectionClosed | Self::Connection(_) | Self::NotRegistered
        )
    }

    /// Check if this error indicates a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::RequestTimeout | Self::PairingTimeout)
    }

    /// Check if this error indicates an unsupported operation.
    pub fn is_not_supported(&self) -> bool {
        matches!(self, Self::NotSupported { .. })
    }

    /// Check if this is a pairing-related error.
    pub fn is_pairing_error(&self) -> bool {
        matches!(
            self,
            Self::PairingRejected
                | Self::PairingTimeout
                | Self::NotRegistered
                | Self::PairingInProgress(_)
                | Self::PairingCancelled
                | Self::InvalidPairingInput(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_supported_error() {
        let err = Error::not_supported("Screen blanking not available", Capabilities::SCREEN);

        assert!(err.is_not_supported());
        assert!(err.to_string().contains("screen"));
        assert!(err.to_string().contains("Screen blanking"));
    }

    #[test]
    fn test_backend_error() {
        let err = Error::backend("lg", "WebSocket handshake failed");

        assert!(err.to_string().contains("lg"));
        assert!(err.to_string().contains("WebSocket handshake"));
    }

    #[test]
    fn test_is_not_connected() {
        assert!(Error::ConnectionClosed.is_not_connected());
        assert!(Error::NotRegistered.is_not_connected());
        assert!(!Error::RequestTimeout.is_not_connected());
    }

    #[test]
    fn test_is_timeout() {
        assert!(Error::RequestTimeout.is_timeout());
        assert!(Error::PairingTimeout.is_timeout());
        assert!(!Error::ConnectionClosed.is_timeout());
    }

    #[test]
    fn test_is_pairing_error() {
        assert!(Error::PairingRejected.is_pairing_error());
        assert!(Error::PairingTimeout.is_pairing_error());
        assert!(Error::PairingCancelled.is_pairing_error());
        assert!(Error::PairingInProgress("tv1".to_string()).is_pairing_error());
        assert!(!Error::ConnectionClosed.is_pairing_error());
    }
}
