//! In-memory mock backend for tisvlay.
//!
//! `tisvlay-mock` implements the [`TvBackend`], [`TvConnector`], and
//! [`TvConnection`] traits from `tisvlay-core` with a fully in-process
//! state machine. It is useful for:
//!
//! - **Development** of UX/UI features (tray, CLI, future GUI) without
//!   needing a real TV on the network
//! - **Integration testing** the daemon against a realistic backend
//!   that exercises the full subscription / state-change pipeline
//! - **CI** runs where no TV hardware is available
//!
//! # Production vs test registration
//!
//! - **Production**: [`tisvlay_backends::create_registry()`] registers
//!   a mock whose state is private. Discovery returns an empty list;
//!   pairing falls back to the config shorthand. Users who want mock
//!   TVs add them manually via their config file.
//! - **Tests**: [`create_test_registry()`] returns a
//!   `(BackendRegistry, MockHandle)` pair sharing a fresh state.
//!   Tests drive discovery and pairing via the handle; no global
//!   state means tests run in parallel by construction.
//!
//! [`TvBackend`]: tisvlay_core::TvBackend
//! [`TvConnector`]: tisvlay_core::TvConnector
//! [`TvConnection`]: tisvlay_core::TvConnection
//! [`tisvlay_backends::create_registry()`]: https://docs.rs/tisvlay-backends
//!
//! # Design notes
//!
//! - `Transport::Network` — scoped by `is_network_dependent()` so
//!   carrier/route events still apply to mock TVs.
//! - `survives_source_ip_change() -> false` — matches LG's behavior,
//!   so NIC-failover tests exercise the same reconnection path.
//! - The "address" is a virtual IP string stored in the vendor config;
//!   no network activity actually happens.
//! - Pairing is a state machine driven either by a config shorthand
//!   (`[tv.mock] pairing = "accept"`) or by explicit test calls
//!   through [`MockHandle`].

mod backend;
mod config;
mod connection;
mod connector;
mod discovery;
mod handle;
mod pairing;
mod state;

use parking_lot::Mutex;
use std::sync::Arc;

pub use backend::MockBackend;
pub use config::MockVendorConfig;
pub use connection::MockConnection;
pub use connector::MockConnector;
pub use discovery::MockDiscovery;
pub use handle::MockHandle;
pub use pairing::PairingShorthand;

use state::MockState;

/// Vendor identifier for the mock backend.
pub const VENDOR_ID: &str = "mock";

/// Human-readable name for the mock backend.
pub const VENDOR_NAME: &str = "Mock (testing)";

/// Build a [`MockConnector`] and [`MockDiscovery`] pair that share a
/// fresh, empty state, along with a [`MockHandle`] for driving them.
///
/// Used by [`tisvlay_backends::create_test_registry()`] and by any
/// tests that want to construct a registry from scratch without
/// going through `tisvlay-backends`.
///
/// In production code, use [`new_production_parts()`] instead —
/// that returns the connector and discovery without a handle, so
/// there's no way to accidentally drive test hooks from production.
pub fn new_test_parts() -> (MockConnector, MockDiscovery, MockHandle) {
    let state = Arc::new(Mutex::new(MockState::new()));
    let connector = MockConnector::from_state(Arc::clone(&state));
    let discovery = MockDiscovery::new(Arc::clone(&state));
    let handle = MockHandle::new(state);
    (connector, discovery, handle)
}

/// Build a [`MockConnector`] and [`MockDiscovery`] pair for production
/// registration.
///
/// The underlying state is fresh and inaccessible — there is no
/// handle, so production code cannot drive test hooks. Discovery
/// always returns an empty list; users who want to use the mock
/// backend in a real deployment add TVs via their config file and
/// rely on the pairing shorthand.
pub fn new_production_parts() -> (MockConnector, MockDiscovery) {
    let state = Arc::new(Mutex::new(MockState::new()));
    let connector = MockConnector::from_state(Arc::clone(&state));
    let discovery = MockDiscovery::new(state);
    (connector, discovery)
}
