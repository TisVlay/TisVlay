#![doc = include_str!("../README.md")]
// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

// Modules
mod capabilities;
mod config;
mod error;
pub mod errors;
mod hardware_id;
mod pairing;
mod pairing_schema;
mod secret;
mod state;
mod traits;
mod vendor_config;
pub mod wol;

// =============================================================================
// Public re-exports: Core abstractions
// =============================================================================

// Traits
pub use traits::{
    LocalDevice, RetryConfig, Transport, TvBackend, TvConnection, TvConnector, TvDiscovery,
};

// Capabilities
pub use capabilities::Capabilities;

// Hardware identity
pub use hardware_id::HardwareId;

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
    SystemProfileConfig, USER_PROFILE_FIELDS, UserProfileConfig,
};

// State
pub use state::{DiscoveredTv, InputInfo, PowerState, TvState};

// Errors
pub use error::{Error, Result};

// Secret values (credentials)
pub use secret::Secret;

// Vendor-specific config tables
pub use vendor_config::VendorConfig;

// Wake-on-LAN
pub use wol::{MAC_CONFIG_KEY, MacAddress};
