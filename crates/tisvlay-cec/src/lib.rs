#![doc = include_str!("../README.md")]
// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

mod address;
mod backend;
mod config;
mod connection;
mod connector;
mod discovery;
mod identity;
mod link;

pub use address::{ParsePhysicalAddressError, PhysicalAddress};
pub use backend::CecBackend;
pub use config::CecVendorConfig;
pub use connection::CecConnection;
pub use connector::CecConnector;
pub use discovery::CecDiscovery;

/// Vendor identifier for the CEC backend (the `[tv.cec]` config key).
pub const VENDOR_ID: &str = "cec";

/// Human-readable name for the CEC backend.
pub const VENDOR_NAME: &str = "HDMI CEC";

/// Out-of-crate test seam (`test-link` feature): the scripted CEC link
/// doubles and the power vocabulary they speak.
///
/// Test-only: `PowerStatus` is linux-cec's type, re-exported so a test can
/// script a fake without depending on linux-cec. The feature is outside the
/// crate's semver surface and follows linux-cec's releases.
#[cfg(any(test, feature = "test-link"))]
pub use link::{FakeCecLink, FakeCecLinkOpener};
#[cfg(any(test, feature = "test-link"))]
pub use linux_cec::operand::PowerStatus;
