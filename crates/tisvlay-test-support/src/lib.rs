//! Shared test harness for the tisvlay workspace.
//!
//! This crate is **dev-only**. It is never shipped in release builds
//! and its dependencies (zbus, tempfile, `dbus-daemon` as a runtime
//! requirement) only matter at `cargo test` time.
//!
//! # What's here
//!
//! - [`PrivateBus`] — spawns a private `dbus-daemon` scoped to a
//!   single test binary, exposes its address, and tears it down on
//!   drop. Each test binary gets exactly one via [`PrivateBus::shared`].
//! - [`fake_logind::FakeLogind`] — a hand-rolled stub of
//!   `org.freedesktop.login1.Manager` that emits `PrepareForSleep`
//!   signals. Register it on a [`PrivateBus`] and drive it from tests.
//!
//! # Why a private bus instead of `dbusmock`?
//!
//! Rust-only toolchain, no Python test dependency, and the zbus
//! `ObjectServer` API is expressive enough to hand-roll the few
//! system services we care about (logind today; more later if we
//! need them). The private bus also gives us binary-level isolation
//! for free: nextest runs one process per test binary, so each
//! binary's `dbus-daemon` is scoped to one process and can't race
//! with any other test binary's bus.
//!
//! # Parallelism
//!
//! Tests within a single binary share one bus. That's fine for tests
//! that use unique object paths (e.g., per-TV under the same daemon
//! service), but tests that both want to own `org.tisvlay.Daemon`
//! cannot run in parallel on one bus — name ownership is bus-global.
//! For such tests, use nextest's `test-groups` to serialize them
//! within the binary.

pub mod dbus;
pub mod fake_logind;

pub use dbus::PrivateBus;
