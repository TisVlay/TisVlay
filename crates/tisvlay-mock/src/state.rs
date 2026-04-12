//! Shared state for the mock backend.
//!
//! `MockState` is the single source of truth for everything a test can
//! control about the mock backend: which TVs are "on the network"
//! (discoverable), and the current state of any in-flight pairing
//! sessions. It is wrapped in an `Arc<Mutex<MockState>>` and shared
//! between [`MockConnector`](crate::MockConnector) (which reads it
//! during `discover()` / `start_pairing()` / `connect()`) and
//! [`MockHandle`](crate::MockHandle) (which tests use to drive the
//! state explicitly).
//!
//! # Concurrency
//!
//! The state is guarded by a `std::sync::Mutex`, not a
//! `tokio::sync::Mutex`. Critical sections are brief (single-method
//! scope) and must never span an `.await` point. Tests call handle
//! methods synchronously between awaits, so there is no deadlock
//! risk by construction. Using `std::sync::Mutex` avoids pulling
//! tokio's async-aware mutex into hot code paths and matches how
//! real backends keep their internal state.
//!
//! # Invariants
//!
//! - Keys in `pairing_outcomes` are TV IDs (same string space as
//!   `DiscoveredTv::id`). A pairing outcome set by the handle before
//!   a pairing session starts will be consumed by the next
//!   `start_pairing()` call for that TV.
//! - `discovered` is a `Vec`, not a `HashMap`, because order matters
//!   to some tests (e.g., "pick the first discovered TV").

use std::collections::HashMap;
use tisvlay_core::DiscoveredTv;

use crate::pairing::PairingOutcome;

/// All test-controllable state for the mock backend.
#[derive(Debug, Default)]
pub(crate) struct MockState {
    /// TVs currently "visible on the network". Returned in order by
    /// `MockDiscovery::discover()`.
    pub(crate) discovered: Vec<DiscoveredTv>,

    /// Pre-seeded pairing outcomes keyed by TV ID. When
    /// `MockConnector::start_pairing()` runs for a TV whose ID is in
    /// this map, the outcome is consumed and drives the pairing
    /// state machine. If no outcome is set, pairing waits for an
    /// explicit `simulate_user_*` call via the handle.
    pub(crate) pairing_outcomes: HashMap<String, PairingOutcome>,
}

impl MockState {
    /// Create an empty state.
    pub(crate) fn new() -> Self {
        Self::default()
    }
}
