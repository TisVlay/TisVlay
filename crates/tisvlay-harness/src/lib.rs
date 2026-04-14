//! Integration-test harness for the tisvlay workspace.
//!
//! Three layers combine here:
//!
//! - [`tisvlay_test`] (workspace) — OS-environment fakes: private
//!   dbus-daemon, fake logind, fake screensaver.
//! - [`tisvlay_mock`] (workspace) — in-memory fake TV backend.
//! - this crate — composes the two into full binary-lifecycle fixtures
//!   ([`daemon::TestDaemon`], [`session::TestSession`],
//!   [`tray::TestTray`]).
//!
//! Each fixture boots the corresponding binary crate against a
//! [`tisvlay_test::PrivateBus`], registers the mock backend, and
//! exposes enough of the running service to drive it from integration
//! tests. Cleanup is handled in `Drop` so tests stay terse.
//!
//! This crate is `publish = false` and reaches into the
//! `#[doc(hidden)] __internals` modules of the binary crates. Don't
//! depend on it from published code.

pub mod common;
pub mod daemon;
pub mod session;
pub mod tray;
