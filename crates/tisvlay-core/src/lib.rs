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
//! ├── secret: Debug-redacted credential values
//! ├── vendor_config: the vendor-specific config table
//! └── wol: Wake-on-LAN (generic)
//!
//! tisvlay-lg (separate crate)
//! └── LG WebOS implementation of traits
//! ```

// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

// Modules
mod capabilities;
mod coalescer;
mod config;
mod desktop;
mod error;
pub mod errors;
mod hardware_id;
mod pairing;
mod pairing_schema;
mod profile_refs;
mod reconciler;
mod refresh_trigger;
mod secret;
mod state;
mod traits;
mod vendor_config;
pub mod wol;

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
    EventAction, ResolvedSystemProfile, ResolvedUserProfile, SYSTEM_PROFILE_FIELDS, SessionAction,
    SystemProfileConfig, USER_PROFILE_FIELDS, UserConfig, UserProfileConfig, UserSection,
    UserTvConfig, UserTvProfileAssignment, absolutize,
};

// Profile-reference computation (used by daemon + clients)
pub use profile_refs::{TvAssignment, references_of, user_references_of};

// State
pub use state::{
    DiscoveredTv, InputInfo, PowerCyclePhase, PowerState, TvState, power_state_str_is_on,
};

// Errors
pub use error::{Error, Result};

// Secret values (credentials)
pub use secret::Secret;

// Vendor-specific config tables
pub use vendor_config::VendorConfig;

// Wake-on-LAN
pub use wol::{MAC_CONFIG_KEY, MacAddress, WakeOptions, wake, wake_with_options};

// Concurrency helpers (async tokio utilities)
pub use coalescer::Coalescer;
pub use reconciler::{Reconciler, ReconcilerMap};
pub use refresh_trigger::RefreshTrigger;
