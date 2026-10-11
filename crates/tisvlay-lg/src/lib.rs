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
//! ```no_run
//! use tisvlay_core::{TvConnector, VendorConfig};
//! use tisvlay_lg::{LgConnector, LgVendorConfig};
//!
//! #[tokio::main]
//! async fn main() -> tisvlay_core::Result<()> {
//!     let mut config = LgVendorConfig::new("192.168.1.100");
//!     config.mac = Some("AA:BB:CC:DD:EE:FF".into());
//!     config.set_client_key("your_client_key");
//!     let config = VendorConfig::from_typed(&config).expect("an LG config is a TOML table");
//!
//!     let mut connection = LgConnector::new().connect(&config).await?;
//!     connection.set_volume(20).await?;
//!     connection.close().await?;
//!     Ok(())
//! }
//! ```

// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

mod backend;
mod client;
mod config;
mod connection;
mod connector;
mod endpoints;
mod protocol;
mod transport;

#[cfg(feature = "discovery")]
mod discovery;

#[cfg(test)]
mod protocol_fixtures;
#[cfg(test)]
pub(crate) mod test_server;

// The public surface is what the daemon registers: the connector,
// backend, discovery and typed config. The SSAP client, protocol types and
// endpoint constants stay private, so protocol changes (LG firmware moves)
// never ripple into this crate's semver.
pub use backend::LgBackend;
pub use config::LgVendorConfig;
pub use connector::LgConnector;

#[cfg(feature = "discovery")]
pub use discovery::LgDiscovery;

/// Vendor ID for LG WebOS TVs.
pub const VENDOR_ID: &str = "lg";

/// Human-readable vendor name.
pub const VENDOR_NAME: &str = "LG WebOS";
