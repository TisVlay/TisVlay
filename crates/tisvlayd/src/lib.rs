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
//! - [`test_harness`] (mock feature) — `TestDaemon`, a one-line
//!   constructor that wires `TvManager` + the full D-Bus surface onto
//!   a private bus for integration tests.

pub mod dbus;
pub mod logind;
pub mod netlink;
pub mod registry;
pub mod state_forwarder;
pub mod tv_manager;

#[cfg(feature = "mock")]
pub mod test_harness;
