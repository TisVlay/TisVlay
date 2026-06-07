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
mod action;
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
mod profile_refs;
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
pub use action::ActionExecutor;
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
    DiscoveryConfig, ResolvedSystemProfile, ResolvedUserProfile, SYSTEM_PROFILE_FIELDS,
    SessionAction, SystemConfig, SystemProfileConfig, SystemSection, TvConfig, TvProfileAssignment,
    USER_PROFILE_FIELDS, UserConfig, UserProfileConfig, UserSection, UserTvConfig,
    UserTvProfileAssignment, absolutize,
};
#[cfg(feature = "dbus")]
pub use config::{
    NotificationCategory, NotificationSettingsBody, SystemProfileBody, UserProfileBody,
};

// Profile-reference computation (used by daemon + clients)
pub use profile_refs::{TvAssignment, references_of, user_references_of};

// State
pub use state::{
    DiscoveredTv, InputInfo, PowerCyclePhase, PowerState, TvState, power_state_str_is_on,
};

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
    DAEMON_SERVICE, MANAGER_PATH, SESSION_INTERFACE, SESSION_PATH, SESSION_SERVICE, TV_BASE_PATH,
    tv_path,
};

#[cfg(feature = "dbus")]
pub use dbus_client::{
    BusKind, DaemonClient, GuiClient, PairTerminal, ServiceEvent, SessionClient, StateEvent,
    StateWatcher, StateWatcherConfig, TrayClient, TvInfo, TvStateChange, add_signal_match,
    subscribe_signals, watch_service,
};
