//! `tisvlay-world` integration tests: one test binary, one module per suite
//! (docs/testing.md, "One integration-test binary per crate").

#![expect(
    clippy::panic,
    reason = "integration tests fail loudly by design; clippy's allow-*-in-tests keys don't reach helper fns under tests/"
)]

mod support;

mod carrier;
mod device;
mod dispatch;
mod held_wake;
mod pairing_profiles;
mod power_edge;
mod reload;
mod remote_keys;
mod store;
mod wake_list;
mod world;
