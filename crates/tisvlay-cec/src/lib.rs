//! HDMI-CEC vendor backend for tisvlay.
//!
//! Controls TVs over the HDMI Consumer Electronics Control bus through the
//! Linux CEC subsystem (`/dev/cecN`), built on Valve's `linux-cec`
//! library. Unlike the network backends, the adapter is reachable on the
//! bus even while the TV is in standby, so power-on is a real on-bus
//! command (`Image View On`) rather than Wake-on-LAN — and a standby TV
//! stays *connected* (`reachable_in_standby`), with the daemon's `power_on`
//! waking it by its reported power state rather than by connect success.
//!
//! v1 capability surface is power (on/off/state) and active-source input
//! switching (`Set Stream Path`). Volume over CEC is relative-only and
//! deferred; backlight and OLED screen-blank are not CEC concepts.
//!
//! The `link` module is the testability seam: production wraps
//! `linux_cec::AsyncDevice`, tests use a scripted fake, so the
//! backend builds and tests with no `/dev/cecN` present.

// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

mod backend;
mod config;
mod connection;
mod connector;
mod discovery;
mod identity;
mod link;

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
#[cfg(any(test, feature = "test-link"))]
pub use link::{FakeCecLink, FakeCecLinkOpener};
#[cfg(any(test, feature = "test-link"))]
pub use linux_cec::operand::PowerStatus;
