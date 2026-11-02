#![doc = include_str!("../README.md")]
// Every public item is documented, which keeps the crate ready for
// crates.io (docs/build.md, first-publish checklist).
#![warn(missing_docs)]

mod address;
mod backend;
mod config;
mod confirm;
mod connection;
mod connector;
mod discovery;
mod identity;
mod link;
pub mod wakeup;

pub use address::{ParsePhysicalAddressError, PhysicalAddress};
pub use backend::CecBackend;
pub use config::CecVendorConfig;
pub use connector::CecConnector;
pub use discovery::CecDiscovery;

/// Vendor identifier for the CEC backend (the `[tv.cec]` config key).
pub const VENDOR_ID: &str = "cec";

/// Human-readable name for the CEC backend.
pub const VENDOR_NAME: &str = "HDMI CEC";

/// Out-of-crate test seam (`test-link` feature): the scripted CEC link
/// doubles and the vocabulary they speak.
///
/// Test-only: `PowerStatus` and `VendorId` are linux-cec's types,
/// re-exported so a test can script a fake without depending on
/// linux-cec. The feature is outside the crate's semver surface and
/// follows linux-cec's releases.
#[cfg(any(test, feature = "test-link"))]
pub use link::{FakeCecLink, FakeCecLinkOpener};
#[cfg(any(test, feature = "test-link"))]
pub use linux_cec::{VendorId, operand::PowerStatus};
