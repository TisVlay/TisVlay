//! `MockConnector` — factory for [`MockConnection`] instances.
//!
//! The connector shares a `MockState` with [`MockHandle`](crate::MockHandle)
//! so test code can drive discovery and pairing outcomes out-of-band.
//! Production builds use [`MockConnector::new()`], which creates a
//! fresh state with no handle; the mock is registered but has an
//! empty network and no pre-seeded pairing outcomes.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use tisvlay_core::{
    Credentials, Error, PairingCommand, PairingError, PairingSession, PairingState, Result,
    Transport, TvConnection, TvConnector,
};
use tracing::instrument;

use crate::config::MockVendorConfig;
use crate::connection::MockConnection;
use crate::pairing::PairingOutcome;
use crate::state::MockState;

/// Factory for mock TV connections.
///
/// Holds a shared [`MockState`] used for discovery and pairing
/// outcome lookup.
#[derive(Debug)]
pub struct MockConnector {
    pub(crate) state: Arc<Mutex<MockState>>,
}

impl MockConnector {
    /// Create a new mock connector with a fresh, empty state.
    ///
    /// Used by production `create_registry()` — the handle is
    /// implicitly dropped, so test hooks are inaccessible.
    pub fn new() -> Self {
        Self::from_state(Arc::new(Mutex::new(MockState::new())))
    }

    /// Wrap an existing shared state. Used by
    /// [`create_test_registry`](crate::create_test_registry) so
    /// the connector and handle share state.
    pub(crate) fn from_state(state: Arc<Mutex<MockState>>) -> Self {
        Self { state }
    }
}

impl Default for MockConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a `toml::Value` into a [`MockVendorConfig`], with helpful errors.
fn parse_config(value: &toml::Value) -> Result<MockVendorConfig> {
    value
        .clone()
        .try_into::<MockVendorConfig>()
        .map_err(|e| Error::backend(crate::VENDOR_ID, format!("invalid mock config: {e}")))
}

#[async_trait]
impl TvConnector for MockConnector {
    fn transport(&self) -> Transport {
        // Even though the mock doesn't use actual network traffic, we
        // report Network so carrier/route events still apply to it.
        // This lets network-monitoring tests exercise the real code
        // paths (InterfaceDown → disconnect → InterfaceUp → reconnect).
        Transport::Network
    }

    fn survives_source_ip_change(&self) -> bool {
        // Match LG's behavior so NIC-failover tests follow the same
        // reconnect path the real backend would.
        false
    }

    fn reachable_in_standby(&self, vendor_config: &toml::Value) -> bool {
        // Test-configurable: models a CEC-style bus that stays reachable in
        // standby (the connection survives power-off). Default `false` so
        // existing network-shaped mock tests are unaffected.
        parse_config(vendor_config)
            .map(|c| c.reachable_in_standby)
            .unwrap_or(false)
    }

    fn emulate_screen_via_power(&self, vendor_config: &toml::Value) -> bool {
        // Test-configurable: models a backend (CEC) with no discrete
        // screen-blank, so the daemon dispatches screen actions as power.
        // Default `false` so existing mock tests keep their real SCREEN.
        parse_config(vendor_config)
            .map(|c| c.emulate_screen_via_power)
            .unwrap_or(false)
    }

    #[instrument(skip(self, vendor_config))]
    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = parse_config(vendor_config)?;

        // Honour any test-injected connect delay (simulates slow/unreachable TV),
        // pick up any test-seeded hardware identity keyed by address, and bump
        // the per-address connect counter so tests can assert an idempotent
        // `power_on` skipped the teardown/reconnect.
        let (delay, hardware_id, connect_counter) = {
            let mut state = self.state.lock();
            let counter = state
                .connect_calls
                .entry(config.address.clone())
                .or_default()
                .clone();
            (
                state.connect_delay,
                state.hardware_ids.get(&config.address).cloned(),
                counter,
            )
        };
        connect_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(delay) = delay {
            tracing::debug!(address = %config.address, ?delay, "delaying mock connect");
            tokio::time::sleep(delay).await;
        }

        if config.refuse_connect {
            return Err(tisvlay_core::Error::backend(
                crate::VENDOR_ID,
                "connect refused by test config",
            ));
        }
        tracing::debug!(address = %config.address, "connecting to mock TV");
        Ok(Box::new(MockConnection::new(
            &config,
            hardware_id,
            self.state.clone(),
        )))
    }

    fn supports_wake(&self, vendor_config: &toml::Value) -> bool {
        // Config-driven so tests can flip it via update-config; wake
        // itself stays a no-op either way.
        parse_config(vendor_config).map(|c| c.wake).unwrap_or(true)
    }

    #[instrument(skip(self, vendor_config))]
    async fn wake(&self, vendor_config: &toml::Value) -> Result<()> {
        let config = parse_config(vendor_config)?;
        tracing::debug!(address = %config.address, "mock wake (no-op)");
        Ok(())
    }

    async fn wake_and_connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        // Wake is a no-op for the mock, so this is just `connect`.
        self.connect(vendor_config).await
    }

    #[instrument(skip(self, vendor_config))]
    fn start_pairing(&self, vendor_config: &toml::Value) -> Result<PairingSession> {
        // Parse the config to validate it and extract the pairing-mode
        // shorthand. Pairing outcomes can come from two places:
        //
        //   1. The config shorthand (`pairing = "accept"` etc.), applied
        //      here by seeding the state before dispatch.
        //   2. A prior call to `MockHandle::simulate_user_*`, which set
        //      an outcome on the state directly.
        //
        // If both are set, the handle wins: it's stored directly and the
        // shorthand's would-be-seeded value is overwritten only if no
        // outcome is already present.
        let config = parse_config(vendor_config)?;
        let address = config.address.clone();

        // Tv ID is derived from the address for mock TVs. In production
        // configs this mapping is one-to-one.
        let tv_id = config.address.clone();

        // Look up any pre-seeded outcome.
        let outcome = {
            let mut state = self.state.lock();
            // If the handle already set an outcome for this TV, take it.
            // Otherwise, fall back to the config shorthand (if any).
            state
                .pairing_outcomes
                .remove(&tv_id)
                .or_else(|| config.pairing.to_outcome())
        };

        tracing::debug!(?outcome, %address, "starting mock pairing");

        let (session, mut command_rx, state_tx) = PairingSession::create();

        tokio::spawn(async move {
            // Announce that the prompt is up — matches LG's behavior.
            let _ = state_tx.send(PairingState::awaiting_tv_prompt(
                "(mock) Please accept the pairing prompt",
            ));

            // Drive the outcome after a tiny yield so the session state
            // channel has time to deliver the "prompt up" event first.
            // Real backends take much longer here; the mock uses zero
            // actual delay so tests run deterministically.
            tokio::task::yield_now().await;

            let final_state = match outcome {
                Some(PairingOutcome::Accept) => {
                    PairingState::complete(Credentials::with_type("mock-key", "client_key"))
                }
                Some(PairingOutcome::Reject) => PairingState::failed(PairingError::rejected()),
                Some(PairingOutcome::Timeout) => PairingState::failed(PairingError::timeout()),
                None => {
                    // No outcome pre-seeded. Wait for a cancel command;
                    // tests that want manual control should set an outcome
                    // before the pairing session starts.
                    tokio::select! {
                        cmd = command_rx.recv() => {
                            match cmd {
                                Some(PairingCommand::Cancel) | None => {
                                    PairingState::failed(PairingError::cancelled())
                                }
                                Some(PairingCommand::SubmitInput(_)) => {
                                    // Mock doesn't use PIN input; succeed.
                                    PairingState::complete(Credentials::with_type(
                                        "mock-key",
                                        "client_key",
                                    ))
                                }
                            }
                        }
                    }
                }
            };

            let _ = state_tx.send(final_state);
        });

        Ok(session)
    }

    fn build_config_from_pairing(
        &self,
        pairing_params: &toml::Value,
        _credentials: &Credentials,
    ) -> Result<toml::Value> {
        // Credentials are ignored — the mock has nothing to store.
        // Echo the pairing params back so the daemon saves a valid config.
        let config = parse_config(pairing_params)?;
        toml::Value::try_from(&config)
            .map_err(|e| Error::backend(crate::VENDOR_ID, format!("serialize config: {e}")))
    }

    fn config_schema(&self) -> Vec<tisvlay_core::PairingField> {
        use tisvlay_core::{PairingField, PairingFieldType};
        // The mock's other config knobs (`initial_volume`, ramp
        // injectors, etc.) are test-fixture inputs, not user-editable
        // settings, so they don't appear here. `pairing` is the
        // exception — surfaced so the GUI can drive a manual pairing
        // flow without a real TV (and exercise the wizard's
        // reject/timeout paths). Tests that prefer handle-driven
        // pairing pass `pairing = "manual"` in their TOML config.
        vec![
            PairingField {
                name: "address".into(),
                label: "Address".into(),
                helper: "Mock-vendor address (`mock://<id>`).".into(),
                field_type: PairingFieldType::String,
                required: true,
                placeholder: "mock://living_room".into(),
                ..Default::default()
            },
            PairingField {
                name: tisvlay_core::MAC_CONFIG_KEY.into(),
                label: "MAC".into(),
                helper: "Wake-on-LAN MAC (the mock's wake is a no-op).".into(),
                field_type: PairingFieldType::Mac,
                required: false,
                placeholder: "AA:BB:CC:DD:EE:FF".into(),
                ..Default::default()
            },
            PairingField {
                name: "pairing".into(),
                label: "Pairing behavior (mock only)".into(),
                helper: "How the mock resolves a pairing prompt. \
                         'accept' is the friendly default for manual \
                         GUI testing. 'reject' and 'timeout' exercise \
                         the wizard's error paths. 'manual' waits for \
                         a test-handle call (used by integration \
                         tests)."
                    .into(),
                field_type: PairingFieldType::Select,
                required: false,
                default: "accept".into(),
                options: vec![
                    "accept".into(),
                    "reject".into(),
                    "timeout".into(),
                    "manual".into(),
                ],
                ..Default::default()
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // start_pairing accepts already-credentialed vendor_config
    //
    // Re-pair calls `start_pairing` with a TV's existing
    // vendor_config — which may include a `client_key`-like field
    // from a prior pair. Mock's `MockVendorConfig` doesn't have a
    // `client_key` field of its own, but serde lenience (no
    // `deny_unknown_fields`) means extra fields parse fine. Verify both
    // a clean re-pair-shaped config and a config with unknown extras are
    // accepted, and that calling twice in succession is idempotent.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn start_pairing_accepts_credentialed_vendor_config() {
        let connector = MockConnector::new();

        // `pairing = "accept"` so the spawned task terminates immediately
        // rather than hanging on cancel-receive (default `manual` mode).
        let mut table = toml::value::Table::new();
        table.insert(
            "address".into(),
            toml::Value::String("mock://repair_verify".into()),
        );
        table.insert("pairing".into(), toml::Value::String("accept".into()));
        // Extra credentialing-shaped field that re-pair might carry through
        // from a prior pair; the mock's serde is lenient so this just gets
        // ignored on parse.
        table.insert(
            "client_key".into(),
            toml::Value::String("preexisting-mock-key".into()),
        );
        let vendor_config = toml::Value::Table(table);

        let session1 = connector.start_pairing(&vendor_config);
        assert!(
            session1.is_ok(),
            "first start_pairing with credentialed config should return Ok; got {:?}",
            session1.as_ref().err(),
        );
        drop(session1);

        let session2 = connector.start_pairing(&vendor_config);
        assert!(
            session2.is_ok(),
            "second start_pairing with the same credentialed config should return Ok; got {:?}",
            session2.as_ref().err(),
        );
    }
}
