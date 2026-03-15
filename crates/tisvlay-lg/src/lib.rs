//! tisvlay-lg - LG WebOS TV backend for tisvlay.
//!
//! This crate provides the LG WebOS implementation of the tisvlay TV control traits.
//! It communicates with LG WebOS TVs using the SSAP (Simple Service Access Protocol)
//! over WebSocket.
//!
//! # Features
//!
//! - Power on/off (Wake-on-LAN for power on)
//! - Screen on/off (OLED-safe blanking)
//! - HDMI input switching
//! - Volume and mute control
//! - Backlight control (OLED pixel brightness)
//! - Network discovery via SSDP
//!
//! # Example
//!
//! ```ignore
//! use tisvlay_lg::{LgBackend, LgConnector, LgConfig};
//! use tisvlay_core::{TvConnector, Capabilities};
//!
//! #[tokio::main]
//! async fn main() -> tisvlay_core::Result<()> {
//!     let connector = LgConnector::new();
//!     let config = LgConfig {
//!         client_key: Some("your_client_key".to_string()),
//!         default_input: None,
//!     };
//!
//!     let mut connection = connector.connect(
//!         "192.168.1.100",
//!         Some("AA:BB:CC:DD:EE:FF"),
//!         &config,
//!     ).await?;
//!
//!     // Use the connection...
//!     connection.close().await?;
//!     Ok(())
//! }
//! ```

mod backend;
mod client;
mod config;
mod connection;
mod endpoints;
mod protocol;

#[cfg(feature = "discovery")]
mod discovery;

// Public re-exports
#[cfg(feature = "discovery")]
pub use backend::LgDiscovery;
pub use backend::{LgBackend, LgConnection, LgConnector};
pub use client::WebOsClient;
pub use config::{LgConfig, LgVendorConfig};
pub use endpoints::{
    audio as audio_endpoints, buttons, input as input_endpoints, picture as picture_endpoints,
    power as power_endpoints, timeouts, WSS_PORT, WS_PORT,
};
pub use protocol::{ExternalInput, PowerState as ProtocolPowerState, VolumeInfo};

#[cfg(feature = "discovery")]
pub use discovery::discover_lg_tvs;

/// Vendor ID for LG WebOS TVs.
pub const VENDOR_ID: &str = "lg";

/// Human-readable vendor name.
pub const VENDOR_NAME: &str = "LG WebOS";
