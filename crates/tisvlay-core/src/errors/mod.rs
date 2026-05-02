//! Per-domain typed error vocabularies.
//!
//! The daemon's error surface is split into per-domain enums,
//! mirroring [ModemManager's
//! `MMCoreError` / `MMConnectionError` / etc. layout][1]:
//!
//! - [`power`] — Wake-on-LAN, connect, and power-cycle failures.
//! - [`config`] — Schema validation, duplicate IDs, profile refs.
//! - Future: `pairing`, `connection` modules.
//!
//! Each variant has:
//! - A typed payload describing the failure
//! - A stable machine-readable id (`PowerError::id()` returns
//!   `"power.no-mac-configured"` etc.) for client-side routing.
//! - A human-readable [`Display`] message via `thiserror`.
//! - Mappings into the daemon's `DaemonError` D-Bus error type
//!   wired up in `tisvlayd::dbus::mod`.
//!
//! Vendor crates (`tisvlay-lg`, future `tisvlay-samsung`) can layer
//! their own backend-specific errors on top via the boxed `source`
//! pattern.
//!
//! [1]: https://www.freedesktop.org/software/ModemManager/doc/latest/ModemManager/ModemManager-Errors.html

pub mod config;
pub mod power;

pub use config::{ConfigError, ConfigErrorCode};
pub use power::{PowerError, PowerErrorCode};
