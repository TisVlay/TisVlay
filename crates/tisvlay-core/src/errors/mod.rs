//! Typed error vocabularies clients route on.
//!
//! - [`config`]: pairing- and config-form validation, carrying the schema
//!   field so the GUI can route the message to its widget.
//! - [`device`]: local device-node access (permission), whose code
//!   reaches clients on the pairing-progress key.

pub mod config;
pub mod device;

pub use config::ConfigError;
pub use device::DeviceErrorCode;
