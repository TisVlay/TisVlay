//! Test-driven control surface for the mock backend.
//!
//! **Mock test-double machinery — not the backend pattern.** A real
//! backend has no `MockHandle`: it talks to hardware instead of an
//! injectable state machine. Don't copy this file when cloning mock as
//! a skeleton for a new backend (see the dev guide's crate-structure
//! section).
//!
//! `MockHandle` is the single entry point by which a test drives
//! everything the mock backend does: publishing discoverable TVs,
//! approving or rejecting pairing sessions, simulating timeouts.
//!
//! Handles are created by [`new_test_parts()`](crate::new_test_parts).
//! Each test gets its own connector/discovery/handle triple sharing a
//! single `Arc<Mutex<MockState>>`, so tests are fully parallel by
//! construction — nothing is global.
//!
//! Production builds never surface the handle: the daemon registers the
//! mock from [`new_production_parts()`], which constructs a fresh state
//! and never creates a handle, so there is no way for production code
//! to accidentally drive test hooks.
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

use parking_lot::Mutex;
use std::sync::Arc;
use tisvlay_core::{DiscoveredTv, HardwareId, PowerState, TvEvent};
use tracing::instrument;

use crate::pairing::PairingOutcome;
use crate::state::MockState;

/// Test-driven control surface for the mock backend.
///
/// Created only by [`new_test_parts()`](crate::new_test_parts); production
/// code never gets one, so it cannot drive test hooks.
#[derive(Clone, Debug)]
pub struct MockHandle {
    state: Arc<Mutex<MockState>>,
}

impl MockHandle {
    /// Wrap a shared state pointer. Only called by
    /// [`new_test_parts`](crate::new_test_parts).
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
        let mut state = self.state.lock();
        tracing::debug!("publishing TV onto mock network");
        state.discovered.push(tv);
    }

    /// Remove all discoverable TVs from the mock "network".
    #[instrument(skip(self))]
    pub fn clear_discovered(&self) {
        let mut state = self.state.lock();
        tracing::debug!(count = state.discovered.len(), "clearing discovered TVs");
        state.discovered.clear();
    }

    /// Number of TVs currently published on the mock network.
    pub fn discovered_count(&self) -> usize {
        self.state.lock().discovered.len()
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
    // Hardware identity
    // ------------------------------------------------------------------

    /// Seed the hardware identity returned by a mock connection opened
    /// to `tv_id` (the mock address, which doubles as the TV ID for mock
    /// TVs). Applied on every connect, not consumed, so tests can query
    /// hardware identity across reconnects without re-seeding.
    #[instrument(skip(self), fields(tv_id = %tv_id))]
    pub fn set_hardware_id(&self, tv_id: &str, id: HardwareId) {
        let mut state = self.state.lock();
        tracing::debug!(?id, "seeding hardware identity");
        state.hardware_ids.insert(tv_id.to_string(), id);
    }

    /// Remove a previously seeded hardware identity.
    pub fn clear_hardware_id(&self, tv_id: &str) {
        let mut state = self.state.lock();
        state.hardware_ids.remove(tv_id);
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
        let mut state = self.state.lock();
        tracing::debug!(?delay, "setting connect delay");
        state.connect_delay = Some(delay);
    }

    /// Remove any previously set connect delay.
    pub fn clear_connect_delay(&self) {
        let mut state = self.state.lock();
        state.connect_delay = None;
    }

    // ------------------------------------------------------------------
    // Live state injection
    // ------------------------------------------------------------------

    /// Push a [`PowerState`] change onto a live mock connection.
    ///
    /// Simulates a TV that changes its own power state without the
    /// daemon having asked — for example, an LG TV that auto-powers-off
    /// after sitting in Screen Off too long. The mutation is applied to
    /// the live `TvState` and pushed through every active subscription
    /// for `address`, so the daemon republishes the cached
    /// `state/power` key the same way it would for a real
    /// backend-initiated state change.
    ///
    /// `address` is the mock vendor address (e.g. `mock://living_room`).
    /// No-op if no connection is currently open for that address.
    #[instrument(skip(self))]
    pub fn inject_power_state(&self, address: &str, target: PowerState) {
        let (state_arc, txs) = {
            let shared = self.state.lock();
            let state_arc = shared.live_states.get(address).cloned();
            let txs = shared
                .subscription_txs
                .get(address)
                .cloned()
                .unwrap_or_default();
            (state_arc, txs)
        };

        let Some(state_arc) = state_arc else {
            tracing::debug!(address, ?target, "no live mock connection to inject into");
            return;
        };

        let snapshot = {
            let mut state = state_arc.lock();
            state.update_power(target);
            state.clone()
        };
        tracing::debug!(address, ?target, "injecting power state into live mock");
        for tx in txs {
            let _ = tx.send(snapshot.clone());
        }
    }

    /// Mutate the live `TvState` for `address` without pushing a
    /// subscription update.
    ///
    /// Simulates the LG firmware's "transition silence" window: when
    /// the user presses Power on the remote, the TV's actual
    /// substate changes immediately (panel goes dark) but the
    /// subscription stream can stay quiet for several seconds before
    /// pushing the new `PowerState`. The daemon's cached
    /// `last_known_power_state` reads stale during that window.
    /// Tests use this knob to verify pre-flight wake gates that
    /// route through an authoritative live query rather than the
    /// stale cached value.
    ///
    /// No-op if no connection is currently open for that address.
    #[instrument(skip(self))]
    pub fn inject_power_state_silent(&self, address: &str, target: PowerState) {
        let state_arc = self.state.lock().live_states.get(address).cloned();
        let Some(state_arc) = state_arc else {
            tracing::debug!(
                address,
                ?target,
                "no live mock connection to mutate silently"
            );
            return;
        };
        let mut state = state_arc.lock();
        state.update_power(target);
        tracing::debug!(
            address,
            ?target,
            "mutated mock state without pushing update"
        );
    }

    /// Press the TV remote's power key, aimed at this PC: push
    /// [`TvEvent::RemotePower`] through every live event subscription for
    /// `address`, as a CEC TV's "power off this device" would.
    ///
    /// No-op if no connection has subscribed for that address.
    #[instrument(skip(self))]
    pub fn press_remote_power(&self, address: &str) {
        let txs = self
            .state
            .lock()
            .event_txs
            .get(address)
            .cloned()
            .unwrap_or_default();
        if txs.is_empty() {
            tracing::debug!(address, "no live mock event subscription to press into");
        }
        for tx in txs {
            let _ = tx.send(TvEvent::RemotePower);
        }
    }

    /// What the last `set_remote_keys` on `address` asked, if any: whether
    /// the TV remote's keys reach the desktop.
    pub fn remote_keys(&self, address: &str) -> Option<bool> {
        self.state.lock().remote_keys.get(address).copied()
    }

    /// Make `MockConnection::screen_on` return `Ok` without mutating
    /// state for `address` while `enabled` is true. Use this to
    /// drive the daemon's verify-retry-escalate path without the
    /// mock's default `is_screen_on=true` flip letting verify pass
    /// on attempt #1. Mirrors LG firmware's behavior when `screen_on`
    /// lands on a TV in `ActiveStandby` / `Standby` substate — the
    /// call succeeds at the wire level but the panel stays dark.
    /// Pass `enabled = false` to restore default mutate-and-push.
    #[instrument(skip(self))]
    pub fn set_screen_on_noop(&self, address: &str, enabled: bool) {
        let mut state = self.state.lock();
        if enabled {
            state.screen_on_noop.insert(address.to_string());
        } else {
            state.screen_on_noop.remove(address);
        }
    }

    /// Parallel knob for `screen_off`. Same semantic: `Ok` return,
    /// no state mutation. Models firmware that ACKs the off command
    /// but doesn't blank the panel. Needed for the reliable-
    /// `screen_off` verify-retry tests.
    #[instrument(skip(self))]
    pub fn set_screen_off_noop(&self, address: &str, enabled: bool) {
        let mut state = self.state.lock();
        if enabled {
            state.screen_off_noop.insert(address.to_string());
        } else {
            state.screen_off_noop.remove(address);
        }
    }

    /// Make an in-band `power_on` meet a closing link: the connection
    /// closes and the call fails with `ConnectionClosed`, as a CEC
    /// adapter unplugged under the command does. Pass `enabled = false`
    /// to restore the default wake in place.
    #[instrument(skip(self))]
    pub fn set_power_on_closes(&self, address: &str, enabled: bool) {
        let mut state = self.state.lock();
        if enabled {
            state.power_on_closes.insert(address.to_string());
        } else {
            state.power_on_closes.remove(address);
        }
    }

    /// Override the `PowerState` a freshly-constructed
    /// `MockConnection` initializes with. Takes precedence over
    /// `MockVendorConfig::initial_power_state` and survives across
    /// reconnects.
    ///
    /// Models LG firmware that refuses to fully wake after a recent
    /// remote-off: the daemon's `power_on` succeeds at the WS level
    /// (a new `MockConnection` is created) but the firmware reports
    /// a wake-preflight substate (`ActiveStandby` / `Standby`) that
    /// never transitions to `Active`. Needed to drive the daemon's
    /// `screen_off`-post-wake-settle graceful-fallback branch.
    #[instrument(skip(self))]
    pub fn set_initial_power_state_override(&self, address: &str, target: PowerState) {
        let mut state = self.state.lock();
        state
            .initial_power_state_overrides
            .insert(address.to_string(), target);
    }

    /// Number of times `MockConnection::screen_on` has been called
    /// for `address`, cumulative across reconnects.
    ///
    /// Lets tests assert the daemon's verify-retry loop actually
    /// iterated instead of passing tautologically when the cached
    /// state happens to match the target before any command was
    /// sent. Returns 0 if the address has never had a connection.
    pub fn screen_on_call_count(&self, address: &str) -> usize {
        self.state
            .lock()
            .screen_on_calls
            .get(address)
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Parallel counter for `MockConnection::screen_off`.
    pub fn screen_off_call_count(&self, address: &str) -> usize {
        self.state
            .lock()
            .screen_off_calls
            .get(address)
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Number of times `MockConnector::connect` has been called for
    /// `address`, cumulative across reconnects.
    ///
    /// Lets tests assert an idempotent `power_on` (live connection, TV
    /// already on) skipped the teardown/reconnect rather than flapping a
    /// healthy link. Returns 0 if the address has never been connected.
    pub fn connect_call_count(&self, address: &str) -> usize {
        self.state
            .lock()
            .connect_calls
            .get(address)
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// How many times the daemon asked connections to the TV at `address`
    /// for their link MAC, cumulative across reconnects. 0 if never.
    pub fn link_mac_call_count(&self, address: &str) -> usize {
        self.state
            .lock()
            .link_mac_calls
            .get(address)
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    fn set_pairing_outcome(&self, tv_id: &str, outcome: PairingOutcome) {
        let mut state = self.state.lock();
        tracing::debug!(?outcome, "seeding pairing outcome");
        state.pairing_outcomes.insert(tv_id.to_string(), outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_test_parts;
    use tisvlay_core::TvConnector;

    /// Build a minimal mock vendor-config table for the given address.
    fn vendor_config(address: &str) -> tisvlay_core::VendorConfig {
        let mut config = tisvlay_core::VendorConfig::new();
        config.insert("address", address);
        config
    }

    /// `set_initial_power_state_override` applies at construct-time
    /// for the *next* `MockConnection` opened to the address — pinning
    /// the precedence-over-vendor-config behavior the
    /// `screen_off`-graceful-fallback test relies on.
    #[tokio::test]
    async fn override_initial_power_state_takes_effect_on_connect() {
        let (connector, _discovery, handle) = new_test_parts();
        let cfg = vendor_config("mock://override_test");

        handle.set_initial_power_state_override("mock://override_test", PowerState::ActiveStandby);

        let mut conn = connector.connect(&cfg).await.expect("connect");
        let power = conn.get_power_state().await.expect("get_power_state");
        assert_eq!(power, PowerState::ActiveStandby);
    }

    /// The override survives reconnects — drops the first connection,
    /// opens a second, and the new one still picks up the override.
    /// Models the LG firmware quirk where `power_on` reconnects but
    /// the TV is still latched in `ActiveStandby`.
    #[tokio::test]
    async fn override_survives_reconnect() {
        let (connector, _discovery, handle) = new_test_parts();
        let cfg = vendor_config("mock://override_reconnect");

        handle.set_initial_power_state_override(
            "mock://override_reconnect",
            PowerState::ActiveStandby,
        );

        let mut first = connector.connect(&cfg).await.expect("first connect");
        assert_eq!(
            first.get_power_state().await.expect("get_power_state"),
            PowerState::ActiveStandby,
        );
        drop(first);

        let mut second = connector.connect(&cfg).await.expect("second connect");
        assert_eq!(
            second.get_power_state().await.expect("get_power_state"),
            PowerState::ActiveStandby,
            "override must survive across `MockConnection` lifetimes",
        );
    }

    /// `screen_on_call_count` starts at zero, increments on every
    /// `MockConnection::screen_on` (including when the noop knob
    /// suppresses the state mutation), and is cumulative across
    /// reconnects to the same address.
    #[tokio::test]
    async fn screen_on_call_count_is_cumulative_across_noop_and_reconnect() {
        let (connector, _discovery, handle) = new_test_parts();
        let cfg = vendor_config("mock://counter_test");

        // Cold counter.
        assert_eq!(handle.screen_on_call_count("mock://counter_test"), 0);

        let mut first = connector.connect(&cfg).await.expect("first connect");
        first.screen_on().await.expect("screen_on #1");
        assert_eq!(handle.screen_on_call_count("mock://counter_test"), 1);

        // Engage the noop knob — counter still bumps even when the
        // command doesn't mutate state. This is the load-bearing
        // invariant for the verify-retry-loop tests.
        handle.set_screen_on_noop("mock://counter_test", true);
        first.screen_on().await.expect("screen_on #2 (noop)");
        assert_eq!(handle.screen_on_call_count("mock://counter_test"), 2);

        drop(first);
        let mut second = connector.connect(&cfg).await.expect("second connect");
        second.screen_on().await.expect("screen_on #3 (reconnect)");
        assert_eq!(
            handle.screen_on_call_count("mock://counter_test"),
            3,
            "counter must be cumulative across reconnects (shared via MockState)",
        );
    }

    /// Parallel coverage for `screen_off_call_count`.
    #[tokio::test]
    async fn screen_off_call_count_bumps_on_noop_too() {
        let (connector, _discovery, handle) = new_test_parts();
        let cfg = vendor_config("mock://off_counter_test");
        let addr = "mock://off_counter_test";

        assert_eq!(handle.screen_off_call_count(addr), 0);

        handle.set_screen_off_noop(addr, true);
        let mut conn = connector.connect(&cfg).await.expect("connect");
        conn.screen_off().await.expect("screen_off");
        conn.screen_off().await.expect("screen_off #2");
        assert_eq!(handle.screen_off_call_count(addr), 2);
    }

    /// Reading the counter for an address that has never had a
    /// connection returns 0 (not a panic, not `Err`).
    #[test]
    fn call_counts_for_unknown_address_return_zero() {
        let (_connector, _discovery, handle) = new_test_parts();
        assert_eq!(handle.screen_on_call_count("mock://never_connected"), 0);
        assert_eq!(handle.screen_off_call_count("mock://never_connected"), 0);
    }

    /// `disable_subscription = true` makes `subscribe_state` return
    /// `NotSupported` (modeling a poll-only backend) while `get_state`
    /// still works — so the daemon's poll path has something to read.
    /// The default (knob absent / false) keeps the push subscription,
    /// which the bulk of the mock-based tests rely on.
    #[tokio::test]
    async fn disable_subscription_makes_subscribe_state_unsupported() {
        let (connector, _discovery, _handle) = new_test_parts();

        // Default config: subscription works.
        let mut push = connector
            .connect(&vendor_config("mock://push"))
            .await
            .expect("connect push");
        assert!(
            push.subscribe_state().await.is_ok(),
            "default mock must keep its push subscription",
        );

        // Poll-only config: subscription refused, but get_state works.
        let mut cfg = tisvlay_core::VendorConfig::new();
        cfg.insert("address", "mock://poll");
        cfg.insert("disable_subscription", true);

        let mut poll = connector.connect(&cfg).await.expect("connect poll");
        let err = poll
            .subscribe_state()
            .await
            .expect_err("poll-only must refuse subscribe_state");
        assert!(
            err.is_not_supported(),
            "poll-only subscribe_state should be NotSupported, got: {err:?}",
        );
        assert!(
            poll.get_state().await.is_ok(),
            "poll-only get_state must still work so the daemon can poll",
        );
    }
}
