//! `MockConnection` — in-memory state machine with push-state subscription.
//!
//! Emulates the behavior of a real TV backend (like LG WebOS) closely enough
//! that the daemon's subscription forwarder, D-Bus property cache, and
//! reconnection logic all exercise real code paths against it.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use tisvlay_core::{
    Capabilities, Error, HardwareId, InputInfo, PowerState, Result, TvConnection, TvState,
};
use tokio::sync::mpsc;

use crate::config::MockVendorConfig;
use crate::state::MockState;

/// Connection to a mock TV.
///
/// All state lives in an `Arc<Mutex<TvState>>` so a background task can
/// push updates through the subscription channel without borrowing
/// conflicts against the caller's `&mut self` during method calls.
#[derive(Debug)]
pub struct MockConnection {
    state: Arc<Mutex<TvState>>,
    /// Shared MockState pointer. Each new connection registers its
    /// `state` Arc and (on subscribe) its subscription `tx` here so
    /// `MockHandle::inject_*` methods can drive state changes outside
    /// the normal command path. Keyed by `address` below.
    shared: Arc<Mutex<MockState>>,
    /// Vendor address (e.g. `mock://living_room`) — the key into
    /// `shared.live_states` / `shared.subscription_txs`.
    address: String,
    connected: bool,
    /// Sender for pushing state updates to subscribers. Cloned into
    /// each state-mutating method so subscribers see cumulative state
    /// after every operation — matching LG's push model.
    subscription_tx: Option<mpsc::UnboundedSender<TvState>>,
    /// Pre-seeded hardware identity applied at connect time. Returned
    /// from [`TvConnection::hardware_id`]; `None` when no test has
    /// seeded one via `MockHandle::set_hardware_id`.
    hardware_id: Option<HardwareId>,
    /// Intermediate values pushed by `set_backlight` before the final
    /// target. Empty by default (snap behavior); populated from
    /// `MockVendorConfig::backlight_ramp_intermediates` to model TVs
    /// that ramp.
    backlight_ramp_intermediates: Vec<u8>,
    /// Delay between ramp steps. 0 → `yield_now` (instant); >0 →
    /// `tokio::time::sleep` so tests can interleave commands mid-ramp.
    backlight_ramp_step_delay: std::time::Duration,
}

impl MockConnection {
    /// Create a new mock connection from a vendor config.
    ///
    /// Initializes state from the config's `initial_*` fields and marks
    /// the connection as live. `hardware_id` is optionally seeded by
    /// the connector from state set via `MockHandle::set_hardware_id`.
    /// Registers the live state pointer in `shared` so
    /// `MockHandle::inject_*` can mutate it; reconnects overwrite the
    /// prior entry.
    pub(crate) fn new(
        config: &MockVendorConfig,
        hardware_id: Option<HardwareId>,
        shared: Arc<Mutex<MockState>>,
    ) -> Self {
        let power = PowerState::parse(&config.initial_power_state);
        let inputs: Vec<InputInfo> = config
            .inputs
            .iter()
            .map(|(id, label)| InputInfo {
                id: id.clone(),
                label: label.clone(),
                connected: true,
            })
            .collect();

        let mut state = TvState::new();
        state.update_power(power);
        state.volume = Some(config.initial_volume);
        state.muted = Some(false);
        state.current_input = Some(config.initial_input.clone());
        state.backlight = Some(config.initial_backlight);
        state.inputs = inputs;
        let state_arc = Arc::new(Mutex::new(state));

        shared
            .lock()
            .live_states
            .insert(config.address.clone(), state_arc.clone());

        Self {
            state: state_arc,
            shared,
            address: config.address.clone(),
            connected: true,
            subscription_tx: None,
            hardware_id,
            backlight_ramp_intermediates: config.backlight_ramp_intermediates.clone(),
            backlight_ramp_step_delay: std::time::Duration::from_millis(
                config.backlight_ramp_step_delay_ms,
            ),
        }
    }

    /// Helper: mutate state under the lock and push the new snapshot
    /// to any active subscriber. Matches LG's behavior where every
    /// backend-initiated change results in a push event.
    fn mutate_and_push(&self, f: impl FnOnce(&mut TvState)) {
        push_state(&self.state, &self.subscription_tx, f);
    }
}

impl Drop for MockConnection {
    /// Remove our shared-state registrations so the subscription
    /// channel's senders all drop together.
    ///
    /// Without this, the cloned sender registered in
    /// `shared.subscription_txs` during `subscribe_state` would
    /// outlive the connection and keep the channel alive forever —
    /// the subscription task's `state_rx.recv()` would never return
    /// `None`, so the `ConnectionChanged(false)` end-of-stream gate in
    /// `tv_manager::run_subscription_task` would never fire. Tests
    /// that assert `Tv1.Connected == false` after `power_off` would
    /// hang or see the wrong value.
    fn drop(&mut self) {
        let mut shared = self.shared.lock();
        shared.live_states.remove(&self.address);
        shared.subscription_txs.remove(&self.address);
    }
}

/// Free-function variant of [`MockConnection::mutate_and_push`] that
/// can be called from a spawned task (which can't capture
/// `&MockConnection` directly).
fn push_state(
    state: &Arc<Mutex<TvState>>,
    subscription_tx: &Option<mpsc::UnboundedSender<TvState>>,
    f: impl FnOnce(&mut TvState),
) {
    let snapshot = {
        let mut state = state.lock();
        f(&mut state);
        state.clone()
    };
    if let Some(tx) = subscription_tx {
        let _ = tx.send(snapshot);
    }
}

#[async_trait]
impl TvConnection for MockConnection {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn get_state(&mut self) -> Result<TvState> {
        Ok(self.state.lock().clone())
    }

    async fn close(&mut self) -> Result<()> {
        self.connected = false;
        self.subscription_tx = None;
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    fn hardware_id(&self) -> Option<HardwareId> {
        self.hardware_id.clone()
    }

    // ------------------------------------------------------------------
    // Power
    // ------------------------------------------------------------------

    async fn power_off(&mut self) -> Result<()> {
        self.mutate_and_push(|s| {
            s.update_power(PowerState::Standby);
        });
        // Real backends close the transport after power_off; mirror that.
        self.connected = false;
        Ok(())
    }

    async fn get_power_state(&mut self) -> Result<PowerState> {
        let state = self.state.lock();
        if state.is_on {
            if state.is_screen_on {
                Ok(PowerState::Active)
            } else {
                Ok(PowerState::ScreenOff)
            }
        } else {
            Ok(PowerState::Standby)
        }
    }

    // ------------------------------------------------------------------
    // Screen
    // ------------------------------------------------------------------

    async fn screen_on(&mut self) -> Result<()> {
        // Tests can opt into "screen_on no-ops, returns Ok" by setting
        // the address in `screen_on_noop` via the handle. Models LG
        // firmware refusing to wake the panel from `ActiveStandby` /
        // `Standby` substates while still returning a non-Err
        // response — the exact condition the daemon's verify-retry-
        // escalate path is built to detect.
        let noop = self.shared.lock().screen_on_noop.contains(&self.address);
        if noop {
            return Ok(());
        }
        // Route through `update_power(Active)` so the raw PowerState
        // field stays consistent with the derived booleans — matches
        // how a real backend reports the substate transition.
        self.mutate_and_push(|s| s.update_power(PowerState::Active));
        Ok(())
    }

    async fn screen_off(&mut self) -> Result<()> {
        // Parallel knob for `screen_off`: tests opt in via
        // `set_screen_off_noop` to model firmware that returns Ok
        // but doesn't blank the panel. Mirrors `screen_on_noop`.
        let noop = self.shared.lock().screen_off_noop.contains(&self.address);
        if noop {
            return Ok(());
        }
        self.mutate_and_push(|s| s.update_power(PowerState::ScreenOff));
        Ok(())
    }

    async fn is_screen_on(&mut self) -> Result<bool> {
        Ok(self.state.lock().is_screen_on)
    }

    // ------------------------------------------------------------------
    // Volume
    // ------------------------------------------------------------------

    async fn set_volume(&mut self, level: u8) -> Result<()> {
        let level = level.min(100);
        self.mutate_and_push(|s| s.volume = Some(level));
        Ok(())
    }

    async fn get_volume(&mut self) -> Result<u8> {
        Ok(self.state.lock().volume.unwrap_or(0))
    }

    async fn set_mute(&mut self, mute: bool) -> Result<()> {
        self.mutate_and_push(|s| s.muted = Some(mute));
        Ok(())
    }

    async fn is_muted(&mut self) -> Result<bool> {
        Ok(self.state.lock().muted.unwrap_or(false))
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        // Validate against the known input list, matching LG's behavior.
        let exists = {
            let state = self.state.lock();
            state.inputs.iter().any(|i| i.id == input_id)
        };
        if !exists {
            return Err(Error::backend(
                crate::VENDOR_ID,
                format!("unknown input: {input_id}"),
            ));
        }
        let id = input_id.to_string();
        self.mutate_and_push(|s| s.current_input = Some(id));
        Ok(())
    }

    async fn get_current_input(&mut self) -> Result<Option<String>> {
        Ok(self.state.lock().current_input.clone())
    }

    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        Ok(self.state.lock().inputs.clone())
    }

    // ------------------------------------------------------------------
    // Backlight
    // ------------------------------------------------------------------

    async fn set_backlight(&mut self, level: u8) -> Result<()> {
        let level = level.min(100);
        let intermediates = self.backlight_ramp_intermediates.clone();
        let delay = self.backlight_ramp_step_delay;

        if delay.is_zero() {
            // Synchronous ramp: push all values inline before
            // returning. Used by emit-ordering tests where the goal
            // is to exercise the daemon's `PropertiesChanged`
            // sequencing under back-to-back pushes; timing doesn't
            // matter, just order.
            for v in intermediates {
                self.mutate_and_push(|s| s.backlight = Some(v.min(100)));
                tokio::task::yield_now().await;
            }
            self.mutate_and_push(|s| s.backlight = Some(level));
        } else {
            // Async ramp: return immediately, push intermediates over
            // time on a spawned task. Models real LG behavior — the
            // Luna `setSystemSettings` roundtrip completes well
            // before the TV finishes its physical backlight ramp, so
            // subscription pushes (`picture/backlight`) arrive AFTER
            // `set_backlight` returns. Tests that need to interleave
            // a new command mid-ramp depend on this asynchrony.
            let state = self.state.clone();
            let subscription_tx = self.subscription_tx.clone();
            tokio::spawn(async move {
                for v in intermediates {
                    push_state(&state, &subscription_tx, |s| s.backlight = Some(v.min(100)));
                    tokio::time::sleep(delay).await;
                }
                push_state(&state, &subscription_tx, |s| s.backlight = Some(level));
            });
        }
        Ok(())
    }

    async fn get_backlight(&mut self) -> Result<u8> {
        Ok(self.state.lock().backlight.unwrap_or(0))
    }

    // ------------------------------------------------------------------
    // Subscriptions
    // ------------------------------------------------------------------

    async fn subscribe_state(&mut self) -> Result<mpsc::UnboundedReceiver<TvState>> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Send an initial snapshot so subscribers see the current state
        // immediately — LG does the same after SSAP subscribe.
        let snapshot = self.state.lock().clone();
        let _ = tx.send(snapshot);
        // Register the sender in shared state so `MockHandle::inject_*`
        // can push state changes to this connection's subscriber.
        self.shared
            .lock()
            .subscription_txs
            .entry(self.address.clone())
            .or_default()
            .push(tx.clone());
        self.subscription_tx = Some(tx);
        Ok(rx)
    }
}
