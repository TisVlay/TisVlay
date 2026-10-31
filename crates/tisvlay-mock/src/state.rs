//! Shared state for the mock backend.
//!
//! **Mock test-double machinery — not the backend pattern.** A real
//! backend keeps its state inside its connection; `MockState` is
//! externalized only so the test handle can drive it. Don't replicate
//! this shape when cloning mock as a skeleton.
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
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tisvlay_core::{DiscoveredTv, HardwareId, PowerState, TvEvent, TvState};
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

    /// Live event senders per mock TV address, registered by
    /// `MockConnection::subscribe_events` for
    /// [`MockHandle::press_remote_power`](crate::handle::MockHandle::press_remote_power).
    pub(crate) event_txs: HashMap<String, Vec<mpsc::UnboundedSender<TvEvent>>>,

    /// The last `set_remote_keys` per mock TV address, for
    /// [`MockHandle::remote_keys`](crate::handle::MockHandle::remote_keys).
    /// Kept across reconnects.
    pub(crate) remote_keys: HashMap<String, bool>,

    /// Addresses where `MockConnection::screen_on` should return `Ok`
    /// without mutating state. Set via
    /// [`MockHandle::set_screen_on_noop`](crate::handle::MockHandle::set_screen_on_noop)
    /// so tests can model LG firmware's "screen_on no-ops when the TV
    /// has slipped past the screen-off substate" behavior — needed to
    /// genuinely exercise the daemon's verify-retry-escalate path
    /// (mock's default flips `is_screen_on=true`, which otherwise
    /// lets verify succeed on the first attempt).
    pub(crate) screen_on_noop: std::collections::HashSet<String>,

    /// Addresses where `MockConnection::screen_off` should return
    /// `Ok` without mutating state. Set via
    /// [`MockHandle::set_screen_off_noop`](crate::handle::MockHandle::set_screen_off_noop)
    /// to model firmware that returns Ok on the wire but doesn't
    /// blank the panel. Needed for V2's reliable-`screen_off`
    /// verify-retry tests, mirroring the `screen_on_noop` knob.
    pub(crate) screen_off_noop: std::collections::HashSet<String>,

    /// Per-address override for the `PowerState` a new
    /// `MockConnection` initializes with at construct-time. When
    /// present, takes precedence over `MockVendorConfig::initial_power_state`
    /// and survives across reconnects.
    ///
    /// Models firmware that refuses to fully wake a TV after a
    /// recent remote-off — the daemon's `power_on` succeeds at the
    /// WS level (a new `MockConnection` is created) but the LG
    /// firmware reports a wake-preflight substate that never
    /// transitions to `Active`. Needed to cover the
    /// `screen_off`-post-wake-settle graceful-fallback branch in
    /// the daemon's reliable screen-dispatch tier.
    pub(crate) initial_power_state_overrides: HashMap<String, PowerState>,

    /// Per-address call counter for `MockConnection::screen_on`
    /// invocations. Incremented before the `screen_on_noop` short-
    /// circuit so tests can assert the daemon's verify-retry loop
    /// actually iterated rather than passing tautologically when
    /// cached state happens to match.
    pub(crate) screen_on_calls: HashMap<String, Arc<AtomicUsize>>,

    /// Parallel counter for `MockConnection::screen_off`. Same
    /// rationale.
    pub(crate) screen_off_calls: HashMap<String, Arc<AtomicUsize>>,

    /// Per-address counter of `MockConnector::connect` invocations, so
    /// tests can assert an idempotent `power_on` did NOT tear down and
    /// rebuild a live connection. Shared across reconnects via shared state.
    pub(crate) connect_calls: HashMap<String, Arc<AtomicUsize>>,

    /// Per-address counter of `get_link_mac` reads, so tests can assert
    /// the daemon asks only on pairing dials.
    pub(crate) link_mac_calls: HashMap<String, Arc<AtomicUsize>>,
}

impl MockState {
    /// Create an empty state.
    pub(crate) fn new() -> Self {
        Self::default()
    }
}
