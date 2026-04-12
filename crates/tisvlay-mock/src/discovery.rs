//! `MockDiscovery` — returns TVs published on the mock "network".
//!
//! The discovery implementation reads from [`MockState::discovered`],
//! which tests populate via [`MockHandle::publish_tv`](crate::MockHandle::publish_tv).
//! An empty state produces an empty discovery result, matching the
//! "no TVs on network" scenario.
//!
//! Production builds register a discovery with an empty, unreachable
//! state (the handle is dropped), so production discovery always
//! returns an empty list — users who want mock TVs add them manually
//! via their config file.
//!
//! [`MockState::discovered`]: crate::state::MockState::discovered

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tisvlay_core::{DiscoveredTv, Result, TvDiscovery};

use crate::state::MockState;

/// Discovery implementation backed by [`MockState::discovered`].
///
/// [`MockState::discovered`]: crate::state::MockState::discovered
pub struct MockDiscovery {
    state: Arc<Mutex<MockState>>,
}

impl MockDiscovery {
    /// Create a new discovery instance backed by the given state.
    ///
    /// Called by [`create_test_registry`](crate::create_test_registry)
    /// and by [`create_production_instance`](crate::create_production_instance)
    /// with a fresh, unreachable state.
    pub(crate) fn new(state: Arc<Mutex<MockState>>) -> Self {
        Self { state }
    }
}

impl TvDiscovery for MockDiscovery {
    async fn discover(&self, _timeout: Duration) -> Result<Vec<DiscoveredTv>> {
        let state = self.state.lock().expect("mock state lock poisoned");
        tracing::debug!(count = state.discovered.len(), "mock discovery");
        Ok(state.discovered.clone())
    }
}
