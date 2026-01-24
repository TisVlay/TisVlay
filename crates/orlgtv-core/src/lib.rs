//! orlgtv-core - Core library for LG WebOS TV control.
//!
//! This crate provides a Rust API for controlling LG WebOS TVs using the
//! SSAP (Simple Service Access Protocol) over WebSocket.
//!
//! # Features
//!
//! - Power on/off (Wake-on-LAN for power on)
//! - Screen on/off (OLED-safe blanking)
//! - HDMI input switching
//! - Volume and mute control
//! - Backlight control (OLED pixel brightness)
//!
//! # Example
//!
//! ```no_run
//! use orlgtv_core::{WebOsClient, Config};
//!
//! #[tokio::main]
//! async fn main() -> orlgtv_core::Result<()> {
//!     // Load configuration
//!     let config = Config::load()?;
//!     let tv = config.default_tv().expect("No TVs configured");
//!
//!     // Connect to TV
//!     let mut client = WebOsClient::connect(&tv.ip, tv.client_key.clone()).await?;
//!
//!     // Set volume to 30
//!     client.set_volume(30).await?;
//!
//!     // Turn off screen (OLED-safe)
//!     client.screen_off().await?;
//!
//!     client.close().await?;
//!     Ok(())
//! }
//! ```

mod client;
mod config;
mod connection;
#[cfg(feature = "discovery")]
mod discovery;
mod endpoints;
mod error;
mod protocol;
mod state;
mod wol;

// Public re-exports
pub use client::{wake_and_connect, wake_and_wait, wake_tv, WebOsClient};
pub use config::{Config, DaemonConfig, SessionAction, SessionConfig, TvConfig};
pub use endpoints::{
    audio as audio_endpoints, buttons, input as input_endpoints, picture as picture_endpoints,
    power as power_endpoints, timeouts, WSS_PORT, WS_PORT,
};
pub use error::{Error, Result};
pub use protocol::{ExternalInput, PowerState as ProtocolPowerState, VolumeInfo};
pub use state::{InputInfo, PowerState, TvState};
pub use wol::{wake, wake_reliable, wake_to_ip, wake_with_options, MacAddress, WakeOptions};

#[cfg(feature = "discovery")]
pub use discovery::{discover_tvs, DiscoveredTv};
