//! `tisvlay-daemon` integration tests: one test binary, one module per suite
//! (docs/testing.md, "One integration-test binary per crate").

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly by design; clippy's allow-*-in-tests keys don't reach helper fns under tests/"
)]

mod access;
mod cec;
mod greeter;
mod liveliness;
mod logind;
mod profiles;
mod supervisor;
mod support;
mod wire;
