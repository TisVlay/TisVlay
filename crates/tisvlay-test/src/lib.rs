//! OS-environment fakes for the tisvlay workspace.
//!
//! This crate is **dev-only**. It is never shipped in release builds
//! and its dependencies (zbus, tempfile, `dbus-daemon` as a runtime
//! requirement) only matter at `cargo test` time.
//!
//! # What's here
//!
//! - [`PrivateBus`] — spawns a private `dbus-daemon` and exposes its
//!   address: one per test process via [`PrivateBus::shared`], or one
//!   per test via [`PrivateBus::isolated`].
//! - [`fake_logind::FakeLogind`] — a hand-rolled stub of
//!   `org.freedesktop.login1.Manager` that emits `PrepareForSleep`
//!   signals. Register it on a [`PrivateBus`] and drive it from tests.
//! - [`Relay`] — a Unix-socket relay between a zenoh client and the
//!   service it dials, which a test can cut, stall, or have drop one
//!   batch, while the service keeps running.
//!
//! # Why a private bus instead of `dbusmock`?
//!
//! Rust-only toolchain, no Python test dependency, and the zbus
//! `ObjectServer` API is expressive enough to hand-roll the few
//! system services we care about (logind today; more later if we
//! need them). The private bus also gives us process-level isolation
//! for free: each test process's `dbus-daemon` is its own, so it can't
//! race with any other process's bus.
//!
//! # Parallelism
//!
//! [`PrivateBus::shared`] is one bus per test process. That's fine for
//! tests that only connect or use unique object paths, but tests that
//! own a well-known name (e.g. `org.freedesktop.login1` for a
//! [`fake_logind::FakeLogind`]) can't share a bus with each other — name
//! ownership is bus-global, and plain `cargo test` runs a binary's tests
//! concurrently in one process. Such tests take a
//! [`PrivateBus::isolated`] bus of their own instead.

pub mod dbus;
pub mod fake_logind;
pub mod relay;
pub mod tmp;

pub use dbus::PrivateBus;
pub use relay::Relay;
pub use tmp::socket_tempdir;

// Re-export tracing test utilities so consumers don't need to track
// these crates in their own Cargo.toml.
//
// `#[traced_test]` installs a per-test subscriber that captures every
// `tracing` event in the test scope. On failure the captured logs are
// dumped to stderr alongside the assertion error; on success they're
// discarded. Inside a `#[traced_test]` function, the macro also brings
// `logs_contain(substr)` and `logs_assert(closure)` into scope for
// in-test assertions on captured log content.
//
// `tracing_mock` exposes the full `expect`/`subscriber` API for tests
// that want to assert on specific spans/events emitted by production
// code (observability regression tests).
pub use tracing_mock;
pub use tracing_test::traced_test;
