//! `MockConnector` — factory for [`MockConnection`] instances.
//!
//! The connector shares a `MockState` with [`MockHandle`](crate::MockHandle)
//! so test code can drive discovery and pairing outcomes out-of-band.
//! Production builds use [`MockConnector::new()`], which creates a
//! fresh state with no handle; the mock is registered but has an
//! empty network and no pre-seeded pairing outcomes.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
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

    #[instrument(skip(self, vendor_config))]
    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = parse_config(vendor_config)?;

        // Honour any test-injected connect delay (simulates slow/unreachable TV).
        let delay = {
            let state = self.state.lock().expect("mock state lock poisoned");
            state.connect_delay
        };
        if let Some(delay) = delay {
            tracing::debug!(address = %config.address, ?delay, "delaying mock connect");
            tokio::time::sleep(delay).await;
        }

        tracing::debug!(address = %config.address, "connecting to mock TV");
        Ok(Box::new(MockConnection::new(&config)))
    }

    fn display_address(&self, vendor_config: &toml::Value) -> String {
        parse_config(vendor_config)
            .map(|c| c.address)
            .unwrap_or_else(|_| "mock://<invalid-config>".to_string())
    }

    fn supports_wake(&self, _vendor_config: &toml::Value) -> bool {
        // Mock TVs are always reachable — "wake" is a no-op that returns
        // immediately, so we claim wake support.
        true
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
            let mut state = self.state.lock().expect("mock state lock poisoned");
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
}
