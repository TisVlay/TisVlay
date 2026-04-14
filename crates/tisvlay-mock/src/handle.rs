//! Test-driven control surface for the mock backend.
//!
//! `MockHandle` is the single entry point by which a test drives
//! everything the mock backend does: publishing discoverable TVs,
//! approving or rejecting pairing sessions, simulating timeouts.
//!
//! Handles are created by [`new_test_parts()`](crate::new_test_parts),
//! typically via `tisvlay_backends::create_test_registry()` which wraps
//! it in a ready-to-use `BackendRegistry`. Each test gets its own
//! `(registry, handle)` pair sharing a single `Arc<Mutex<MockState>>`,
//! so tests are fully parallel by construction — nothing is global.
//!
//! Production builds never surface the handle: [`new_production_parts()`]
//! (which is what `create_registry()` uses) constructs a mock with a
//! fresh state and drops its end of the handle immediately, so there
//! is no way for production code to accidentally drive test hooks.
//!
//! [`new_production_parts()`]: crate::new_production_parts
//!
//! # Example
//!
//! ```no_run
//! use tisvlay_core::DiscoveredTv;
//! use tisvlay_mock::new_test_parts;
//!
//! let (_connector, _discovery, mock) = new_test_parts();
//!
//! // Publish a TV onto the "network".
//! mock.publish_tv(DiscoveredTv::new(
//!     "living_room",
//!     "Living Room G4",
//!     "mock://living_room",
//!     "mock",
//! ));
//!
//! // Pre-approve the next pairing attempt for this TV.
//! mock.simulate_user_accept("mock://living_room");
//! ```

use std::sync::{Arc, Mutex};
use tisvlay_core::DiscoveredTv;
use tracing::instrument;

use crate::pairing::PairingOutcome;
use crate::state::MockState;

/// Test-driven control surface for the mock backend.
///
/// See [the module documentation](crate::handle) for an overview.
#[derive(Clone, Debug)]
pub struct MockHandle {
    state: Arc<Mutex<MockState>>,
}

impl MockHandle {
    /// Wrap a shared state pointer. Only called by
    /// [`create_test_registry`](crate::create_test_registry).
    pub(crate) fn new(state: Arc<Mutex<MockState>>) -> Self {
        Self { state }
    }

    // ------------------------------------------------------------------
    // Discovery
    // ------------------------------------------------------------------

    /// Publish a TV onto the mock "network" so it will be returned
    /// from the next [`TvDiscovery::discover`](tisvlay_core::TvDiscovery)
    /// call.
    ///
    /// Subsequent calls append; publishing is order-preserving.
    #[instrument(skip(self), fields(tv_id = %tv.id, address = %tv.address))]
    pub fn publish_tv(&self, tv: DiscoveredTv) {
        let mut state = self.state.lock().expect("mock state lock poisoned");
        tracing::debug!("publishing TV onto mock network");
        state.discovered.push(tv);
    }

    /// Remove all discoverable TVs from the mock "network".
    #[instrument(skip(self))]
    pub fn clear_discovered(&self) {
        let mut state = self.state.lock().expect("mock state lock poisoned");
        tracing::debug!(count = state.discovered.len(), "clearing discovered TVs");
        state.discovered.clear();
    }

    /// Number of TVs currently published on the mock network.
    pub fn discovered_count(&self) -> usize {
        self.state
            .lock()
            .expect("mock state lock poisoned")
            .discovered
            .len()
    }

    // ------------------------------------------------------------------
    // Pairing
    // ------------------------------------------------------------------

    /// Pre-approve the next pairing attempt for the given TV.
    ///
    /// When `MockConnector::start_pairing()` runs and finds this
    /// outcome in the state, it drives the pairing session to
    /// `Complete(Credentials)` immediately.
    #[instrument(skip(self), fields(tv_id = %tv_id))]
    pub fn simulate_user_accept(&self, tv_id: &str) {
        self.set_pairing_outcome(tv_id, PairingOutcome::Accept);
    }

    /// Pre-reject the next pairing attempt for the given TV.
    ///
    /// The session transitions to `Failed(PairingError::Rejected)`.
    #[instrument(skip(self), fields(tv_id = %tv_id))]
    pub fn simulate_user_reject(&self, tv_id: &str) {
        self.set_pairing_outcome(tv_id, PairingOutcome::Reject);
    }

    /// Simulate a pairing timeout for the next pairing attempt.
    ///
    /// The session transitions to `Failed(PairingError::Timeout)`.
    #[instrument(skip(self), fields(tv_id = %tv_id))]
    pub fn simulate_pairing_timeout(&self, tv_id: &str) {
        self.set_pairing_outcome(tv_id, PairingOutcome::Timeout);
    }

    // ------------------------------------------------------------------
    // Connection behavior
    // ------------------------------------------------------------------

    /// Inject a delay before `MockConnector::connect()` returns.
    ///
    /// Simulates a slow or unreachable TV. Useful for testing that
    /// shutdown signals are respected during in-flight connection
    /// attempts.
    #[instrument(skip(self))]
    pub fn set_connect_delay(&self, delay: std::time::Duration) {
        let mut state = self.state.lock().expect("mock state lock poisoned");
        tracing::debug!(?delay, "setting connect delay");
        state.connect_delay = Some(delay);
    }

    /// Remove any previously set connect delay.
    pub fn clear_connect_delay(&self) {
        let mut state = self.state.lock().expect("mock state lock poisoned");
        state.connect_delay = None;
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    fn set_pairing_outcome(&self, tv_id: &str, outcome: PairingOutcome) {
        let mut state = self.state.lock().expect("mock state lock poisoned");
        tracing::debug!(?outcome, "seeding pairing outcome");
        state.pairing_outcomes.insert(tv_id.to_string(), outcome);
    }
}
