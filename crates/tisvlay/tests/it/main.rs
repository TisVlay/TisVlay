//! `tisvlay` SDK integration tests: one test binary, one module per suite
//! (docs/testing.md, "One integration-test binary per crate").

#![expect(
    clippy::unwrap_used,
    reason = "integration tests fail loudly by design; clippy's allow-*-in-tests keys don't reach helper fns under tests/"
)]

mod client;
mod conformance;
mod fake;
