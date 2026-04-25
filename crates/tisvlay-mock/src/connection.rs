//! `MockConnection` — in-memory state machine with push-state subscription.
//!
//! Emulates the behavior of a real TV backend (like LG WebOS) closely enough
//! that the daemon's subscription forwarder, D-Bus property cache, and
//! reconnection logic all exercise real code paths against it.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tisvlay_core::{
    Capabilities, Error, HardwareId, InputInfo, PowerState, Result, TvConnection, TvState,
};
use tokio::sync::mpsc;

use crate::config::MockVendorConfig;

/// Connection to a mock TV.
///
/// All state lives in an `Arc<Mutex<TvState>>` so a background task can
/// push updates through the subscription channel without borrowing
/// conflicts against the caller's `&mut self` during method calls.
#[derive(Debug)]
pub struct MockConnection {
    state: Arc<Mutex<TvState>>,
    connected: bool,
    /// Sender for pushing state updates to subscribers. Cloned into
    /// each state-mutating method so subscribers see cumulative state
    /// after every operation — matching LG's push model.
    subscription_tx: Option<mpsc::UnboundedSender<TvState>>,
    /// Pre-seeded hardware identity applied at connect time. Returned
    /// from [`TvConnection::hardware_id`]; `None` when no test has
    /// seeded one via `MockHandle::set_hardware_id`.
    hardware_id: Option<HardwareId>,
}

impl MockConnection {
    /// Create a new mock connection from a vendor config.
    ///
    /// Initializes state from the config's `initial_*` fields and marks
    /// the connection as live. `hardware_id` is optionally seeded by
    /// the connector from state set via `MockHandle::set_hardware_id`.
    pub fn new(config: &MockVendorConfig, hardware_id: Option<HardwareId>) -> Self {
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

        Self {
            state: Arc::new(Mutex::new(state)),
            connected: true,
            subscription_tx: None,
            hardware_id,
        }
    }

    /// Helper: mutate state under the lock and push the new snapshot
    /// to any active subscriber. Matches LG's behavior where every
    /// backend-initiated change results in a push event.
    fn mutate_and_push(&self, f: impl FnOnce(&mut TvState)) {
        let snapshot = {
            let mut state = self.state.lock().expect("mock state lock poisoned");
            f(&mut state);
            state.clone()
        };
        if let Some(tx) = &self.subscription_tx {
            // Silently drop if the receiver has been closed — matches LG.
            let _ = tx.send(snapshot);
        }
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
        Ok(self.state.lock().expect("mock state lock poisoned").clone())
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
        let state = self.state.lock().expect("mock state lock poisoned");
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
        self.mutate_and_push(|s| s.is_screen_on = true);
        Ok(())
    }

    async fn screen_off(&mut self) -> Result<()> {
        self.mutate_and_push(|s| s.is_screen_on = false);
        Ok(())
    }

    async fn is_screen_on(&mut self) -> Result<bool> {
        Ok(self
            .state
            .lock()
            .expect("mock state lock poisoned")
            .is_screen_on)
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
        Ok(self
            .state
            .lock()
            .expect("mock state lock poisoned")
            .volume
            .unwrap_or(0))
    }

    async fn set_mute(&mut self, mute: bool) -> Result<()> {
        self.mutate_and_push(|s| s.muted = Some(mute));
        Ok(())
    }

    async fn is_muted(&mut self) -> Result<bool> {
        Ok(self
            .state
            .lock()
            .expect("mock state lock poisoned")
            .muted
            .unwrap_or(false))
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        // Validate against the known input list, matching LG's behavior.
        let exists = {
            let state = self.state.lock().expect("mock state lock poisoned");
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
        Ok(self
            .state
            .lock()
            .expect("mock state lock poisoned")
            .current_input
            .clone())
    }

    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        Ok(self
            .state
            .lock()
            .expect("mock state lock poisoned")
            .inputs
            .clone())
    }

    // ------------------------------------------------------------------
    // Backlight
    // ------------------------------------------------------------------

    async fn set_backlight(&mut self, level: u8) -> Result<()> {
        let level = level.min(100);
        self.mutate_and_push(|s| s.backlight = Some(level));
        Ok(())
    }

    async fn get_backlight(&mut self) -> Result<u8> {
        Ok(self
            .state
            .lock()
            .expect("mock state lock poisoned")
            .backlight
            .unwrap_or(0))
    }

    // ------------------------------------------------------------------
    // Subscriptions
    // ------------------------------------------------------------------

    async fn subscribe_state(&mut self) -> Result<mpsc::UnboundedReceiver<TvState>> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Send an initial snapshot so subscribers see the current state
        // immediately — LG does the same after SSAP subscribe.
        let snapshot = self.state.lock().expect("mock state lock poisoned").clone();
        let _ = tx.send(snapshot);
        self.subscription_tx = Some(tx);
        Ok(rx)
    }
}
