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
//! ├── traits: TvBackend, TvConnection, TvConnector, TvDiscovery
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
//! - `dbus` - D-Bus client for communicating with tisvlayd
//! - `logging` - Shared logging init with journald support for binaries
//!
//! # Example
//!
//! ```ignore
//! use tisvlay_core::DaemonClient;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Connect to the running tisvlayd daemon via D-Bus
//!     let daemon = DaemonClient::new_system().await?;
//!     let tvs = daemon.list_tvs().await?;
//!     for tv in &tvs {
//!         println!("{}: {} ({})", tv.uuid, tv.name, if tv.connected { "connected" } else { "offline" });
//!     }
//!     Ok(())
//! }
//! ```

// Core modules (always available)
mod capabilities;
mod coalescer;
mod config;
pub mod dbus_paths;
mod desktop;
mod error;
pub mod errors;
mod hardware_id;
mod pairing;
mod pairing_schema;
mod reconciler;
mod refresh_trigger;
mod state;
mod traits;
pub mod wol;

// Optional modules
#[cfg(feature = "dbus")]
mod dbus_client;
#[cfg(feature = "logging")]
pub mod logging;

// =============================================================================
// Public re-exports: Core abstractions
// =============================================================================

// Traits
pub use traits::{RetryConfig, Transport, TvBackend, TvConnection, TvConnector, TvDiscovery};

// Capabilities
pub use capabilities::Capabilities;

// Hardware identity
pub use hardware_id::HardwareId;

// Desktop mapping (identity)
pub use desktop::DesktopMapping;

// Pairing
pub use pairing::{
    Credentials, InputType, PairingCommand, PairingError, PairingErrorCode, PairingSession,
    PairingState,
};
pub use pairing_schema::{FieldVisibility, PairingField, PairingFieldType};

// Configuration
pub use config::{
    AutomateWhen, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID, DEFAULT_USER_PROFILE_UUID,
    ResolvedSystemProfile, ResolvedUserProfile, SessionAction, SystemConfig, SystemProfileConfig,
    SystemSection, TvConfig, TvProfileAssignment, UserConfig, UserProfileConfig, UserSection,
    UserTvConfig, UserTvProfileAssignment, absolutize,
};

// State
pub use state::{DiscoveredTv, InputInfo, PowerCyclePhase, PowerState, TvState};

// Errors
pub use error::{Error, Result};

// Wake-on-LAN
pub use wol::{MacAddress, WakeOptions, wake, wake_with_options};

// Concurrency helpers (always available — async tokio utilities,
// not D-Bus-specific).
pub use coalescer::Coalescer;
pub use reconciler::{Reconciler, ReconcilerMap};
pub use refresh_trigger::RefreshTrigger;

// =============================================================================
// Public re-exports: Optional features
// =============================================================================

// D-Bus path constants (always available, no feature gate)
pub use dbus_paths::{
    DAEMON_SERVICE, MANAGER_PATH, OBJECT_MANAGER_PATH, SESSION_INTERFACE, SESSION_PATH,
    SESSION_SERVICE, SYSTEM_PROFILE_BASE_PATH, TV_BASE_PATH, USER_PROFILE_BASE_PATH,
    system_profile_path, tv_path, user_profile_path,
};

#[cfg(feature = "dbus")]
pub use dbus_client::{
    BusKind, DaemonClient, GuiClient, PairTerminal, ProfileSummary, ServiceEvent, SessionClient,
    StateEvent, StateWatcher, StateWatcherConfig, TrayClient, TvInfo, TvStateChange,
    add_signal_match, subscribe_signals, watch_service,
};
