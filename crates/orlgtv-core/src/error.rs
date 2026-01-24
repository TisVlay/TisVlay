//! Error types for orlgtv-core.

use std::io;

/// Result type alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur when communicating with an LG WebOS TV.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// WebSocket connection failed.
    #[error("WebSocket connection failed: {0}")]
    Connection(#[from] tokio_tungstenite::tungstenite::Error),

    /// I/O error (network, file, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Configuration file parsing error.
    #[error("Config error: {0}")]
    Config(#[from] toml::de::Error),

    /// TV rejected the registration/pairing request.
    #[error("TV rejected pairing request")]
    PairingRejected,

    /// TV pairing timed out (user didn't respond to prompt).
    #[error("Pairing timed out - no response from TV")]
    PairingTimeout,

    /// Not registered with the TV (no client key).
    #[error("Not registered with TV - pairing required")]
    NotRegistered,

    /// TV returned an error response.
    #[error("TV error: {0}")]
    TvError(String),

    /// Request timed out waiting for response.
    #[error("Request timed out")]
    RequestTimeout,

    /// WebSocket connection was closed unexpectedly.
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
}
