//! tisvlay-epson - Epson projector backend for tisvlay.
//!
//! Controls Epson projectors (target hardware: Pro Cinema LS12000)
//! over **ESC/VP21 on raw TCP port 3629**. A projector on a
//! living-room PC is the same use case as a TV: power on/off, switch
//! inputs, and blank the light source to preserve laser hours (the
//! projector analog of OLED burn-in protection).
//!
//! # Capabilities
//!
//! - Power on/off (with Wake-on-LAN for waking from deep standby)
//! - Screen on/off via A/V mute (blank the projected image)
//! - Input switching (HDMI / PC / Video / USB / LAN)
//! - Backlight control mapped to laser brightness (`LUMLEVEL`)
//!
//! Volume / mute are absent (the projector is speakerless), and
//! discovery is deferred to a hardware-validation follow-up. State is
//! poll-only — ESC/VP21 has no push channel.
//!
//! # Module layout
//!
//! - [`escvp21`] — the pure protocol codec (handshake, framing, reply
//!   parsing, value tables). I/O-free and table-tested.
//! - [`config`] — [`EpsonVendorConfig`] and the config schema.
//! - [`transport`] — the raw TCP transport (`pub(crate)`).
//! - [`client`] — [`EpsonClient`], the high-level command sequencing.
//! - [`backend`] / [`connector`] / [`connection`] — the
//!   `tisvlay-core` trait implementations.

pub mod escvp21;

mod backend;
mod client;
mod config;
mod connection;
mod connector;
mod transport;

#[cfg(test)]
pub(crate) mod test_server;

// Public re-exports
pub use backend::EpsonBackend;
pub use client::EpsonClient;
pub use config::{EpsonVendorConfig, config_schema};
pub use connection::EpsonConnection;
pub use connector::EpsonConnector;

/// Vendor ID for Epson projectors.
pub const VENDOR_ID: &str = "epson";

/// Human-readable vendor name.
pub const VENDOR_NAME: &str = "Epson Projector";
