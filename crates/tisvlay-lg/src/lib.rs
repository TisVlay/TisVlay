#![doc = include_str!("../README.md")]
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
