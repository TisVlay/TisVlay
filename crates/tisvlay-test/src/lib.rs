//! OS-environment fakes for the tisvlay workspace.
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
//! that use unique object paths, but tests that both want to own the
//! same well-known name (e.g. `org.freedesktop.login1` for a
//! [`fake_logind::FakeLogind`]) cannot run in parallel on one bus —
//! name ownership is bus-global. For such tests, use nextest's
//! `test-groups` to serialize them within the binary.

pub mod dbus;
pub mod fake_logind;
pub mod fake_screensaver;

pub use dbus::PrivateBus;

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
