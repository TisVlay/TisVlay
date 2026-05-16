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
//! The state is guarded by a `parking_lot::Mutex`, not a
//! `tokio::sync::Mutex`. Critical sections are brief (single-method
//! scope) and must never span an `.await` point. Tests call handle
//! methods synchronously between awaits, so there is no deadlock
//! risk by construction. `parking_lot::Mutex` has no poisoning and
//! a smaller / faster implementation than `std::sync::Mutex` for
//! hot code paths, matching how real backends keep their internal
//! state.
//!
//! # Invariants
//!
//! - Keys in `pairing_outcomes` are TV IDs (same string space as
//!   `DiscoveredTv::id`). A pairing outcome set by the handle before
//!   a pairing session starts will be consumed by the next
//!   `start_pairing()` call for that TV.
//! - `discovered` is a `Vec`, not a `HashMap`, because order matters
//!   to some tests (e.g., "pick the first discovered TV").

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tisvlay_core::{DiscoveredTv, HardwareId, TvState};
use tokio::sync::mpsc;

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

    /// Hardware identities pre-seeded by tests, keyed by the mock TV
    /// address (which doubles as the TV ID for mock TVs — see
    /// `MockConnector::start_pairing`). Returned from
    /// [`TvConnection::hardware_id`](tisvlay_core::TvConnection::hardware_id)
    /// on any connection opened to that address.
    pub(crate) hardware_ids: HashMap<String, HardwareId>,

    /// Optional delay injected before `MockConnector::connect()` returns.
    /// Simulates a slow or unreachable TV for testing shutdown behavior.
    pub(crate) connect_delay: Option<Duration>,

    /// Live `TvState` per mock TV address. Inserted when
    /// `MockConnector::connect` opens a connection so
    /// [`MockHandle::inject_power_state`](crate::handle::MockHandle::inject_power_state)
    /// can drive state changes that aren't a direct response to a
    /// daemon command (e.g., the TV auto-power-off after Screen Off).
    /// Reconnects overwrite the prior entry.
    pub(crate) live_states: HashMap<String, Arc<Mutex<TvState>>>,

    /// Live subscription senders per mock TV address. Inserted when
    /// `MockConnection::subscribe_state` is called. Multiple entries
    /// may exist after reconnects; closed senders harmlessly fail their
    /// `send` and are left in place (the maps are bounded by test
    /// lifetime).
    pub(crate) subscription_txs: HashMap<String, Vec<mpsc::UnboundedSender<TvState>>>,
}

impl MockState {
    /// Create an empty state.
    pub(crate) fn new() -> Self {
        Self::default()
    }
}
