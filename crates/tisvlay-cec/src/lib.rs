//! HDMI-CEC vendor backend for tisvlay.
//!
//! Controls TVs over the HDMI Consumer Electronics Control bus through the
//! Linux CEC subsystem (`/dev/cecN`), built on Valve's `linux-cec`
//! library. Unlike the network backends, the adapter is reachable on the
//! bus even while the TV is in standby, so power-on is a real on-bus
//! command (`Image View On`) rather than Wake-on-LAN — and `connect`
//! refuses a standby TV so the daemon's `power_on` takes the wake path.
//!
//! v1 capability surface is power (on/off/state). Active-source input
//! switching is the next slice; volume over CEC is relative-only and
//! deferred; backlight and OLED screen-blank are not CEC concepts.
//!
//! The [`link`] module is the testability seam: production wraps
//! `linux_cec::AsyncDevice`, tests use a scripted fake, so the
//! backend builds and tests with no `/dev/cecN` present.

mod backend;
mod config;
mod connection;
mod connector;
mod link;

pub use backend::CecBackend;
pub use config::CecVendorConfig;
pub use connection::CecConnection;
pub use connector::CecConnector;

/// Vendor identifier for the CEC backend (the `[tv.cec]` config key).
pub const VENDOR_ID: &str = "cec";

/// Human-readable name for the CEC backend.
pub const VENDOR_NAME: &str = "HDMI CEC";
