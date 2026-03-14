//! tisvlay-core - Core library for multi-vendor TV control.
//!
//! This crate provides the core abstractions, traits, and types for controlling
//! TVs from multiple vendors. The actual vendor implementations live in separate
//! crates (e.g., `tisvlay-lg` for LG WebOS TVs).
//!
//! # Architecture
//!
//! ```text
//! tisvlay-core (this crate)
//! ├── traits: TvBackend, TvConnection, feature traits
//! ├── capabilities: Capability flags for feature detection
//! ├── pairing: Unified pairing state machine
//! ├── config: Profile-based configuration
//! ├── state: TV state types
//! ├── error: Vendor-agnostic errors
//! └── wol: Wake-on-LAN (generic)
//!
//! tisvlay-lg (separate crate)
//! └── LG WebOS implementation of traits
//! ```
//!
//! # Features
//!
//! - `discovery` - Network discovery support (SSDP)
//! - `dbus` - D-Bus client for communicating with tisvlayd
//!
//! # Example
//!
//! ```ignore
//! use tisvlay_core::{Config, TvConnector, Capabilities};
//! use tisvlay_lg::LgConnector;
//!
//! #[tokio::main]
//! async fn main() -> tisvlay_core::Result<()> {
//!     // Load configuration
//!     let config = Config::load()?;
//!     let tv = config.default_tv().expect("No TVs configured");
//!
//!     // Connect using the appropriate backend
//!     let connector = LgConnector::new();
//!     let mut connection = connector.connect(
//!         &tv.ip,
//!         tv.mac.as_deref(),
//!         &tv.lg.unwrap_or_default(),
//!     ).await?;
//!
//!     // Check capabilities
//!     if connection.capabilities().contains(Capabilities::VOLUME) {
//!         // Cast to VolumeControl trait to use volume methods
//!         // ...
//!     }
//!
//!     connection.close().await?;
//!     Ok(())
//! }
//! ```

// Core modules (always available)
mod capabilities;
mod config;
mod error;
mod pairing;
mod state;
mod traits;
pub mod wol;

// Optional modules
#[cfg(feature = "dbus")]
mod dbus_client;

// =============================================================================
// Public re-exports: Core abstractions
// =============================================================================

// Traits
pub use traits::{
    BacklightControl, InputControl, PowerControl, ScreenControl, TvBackend, TvConnection,
    TvConnector, TvDiscovery, VolumeControl,
};

// Capabilities
pub use capabilities::Capabilities;

// Pairing
pub use pairing::{
    Credentials, InputType, PairingCommand, PairingError, PairingErrorCode, PairingSession,
    PairingState,
};

// Configuration
pub use config::{Config, DaemonConfig, ProfileConfig, SessionAction, TvConfig};

// State
pub use state::{DiscoveredTv, InputInfo, PowerState, TvState};

// Errors
pub use error::{Error, Result};

// Wake-on-LAN
pub use wol::{wake, wake_reliable, wake_to_ip, wake_with_options, MacAddress, WakeOptions};

// =============================================================================
// Public re-exports: Optional features
// =============================================================================

#[cfg(feature = "dbus")]
pub use dbus_client::{DaemonClient, DAEMON_SERVICE, MANAGER_PATH, TV_BASE_PATH, TvInfo};
