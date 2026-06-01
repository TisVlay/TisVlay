//! Library half of the `tisvlayd` daemon.
//!
//! `main.rs` is a thin entry point that calls into this crate. The
//! library exists primarily so integration tests can construct a
//! daemon instance against a private D-Bus harness without spawning
//! the binary as a subprocess.
//!
//! Public modules:
//!
//! - [`dbus`] — D-Bus interface implementations and the connection
//!   builder used by both the binary and tests.
//! - [`tv_manager`] — `TvManager`, the core orchestration type that
//!   owns TV connections, profile dispatch, and config persistence.
//! - [`registry`] — re-exports the backend registry constructor used
//!   by the daemon at startup.
//! - [`logind`] — logind monitoring (shutdown inhibitor, sleep events).
//! - [`netlink`] — kernel netlink monitoring (carrier, route changes).
//! - [`desktop_probe`] — DRM connector enumeration + EDID-based
//!   matching of managed TVs to physical displays.
//!
//! The `__internals` module is a `#[doc(hidden)]` re-export surface for
//! the out-of-crate test harness ([`tisvlay_harness`](../tisvlay_harness)).
//! Do not depend on it from published code.

pub mod dbus;
pub mod desktop_probe;
pub mod dpms;
mod drm;
pub(crate) mod json_toml;
pub mod logind;
pub mod netlink;
pub(crate) mod power_state;
pub mod registry;
pub mod state_forwarder;
pub mod tv_manager;

/// Re-exports for the integration-test harness crate. Not public API.
#[doc(hidden)]
pub mod __internals {
    pub use crate::dbus::{BusKind, ManagerService, build_dbus_connection};
    pub use crate::state_forwarder::state_change_forwarder;
    pub use crate::tv_manager::TvManager;
}
