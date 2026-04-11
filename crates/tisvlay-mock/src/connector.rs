//! `MockConnector` — factory for [`MockConnection`] instances.

use async_trait::async_trait;
use tisvlay_core::{
    Credentials, Error, PairingCommand, PairingSession, PairingState, Result, Transport,
    TvConnection, TvConnector,
};

use crate::config::MockVendorConfig;
use crate::connection::MockConnection;

/// Factory for mock TV connections.
pub struct MockConnector;

impl MockConnector {
    /// Create a new mock connector.
    pub fn new() -> Self {
        Self
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

    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = parse_config(vendor_config)?;
        tracing::debug!(address = %config.address, "Mock: connecting");
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

    async fn wake(&self, vendor_config: &toml::Value) -> Result<()> {
        let config = parse_config(vendor_config)?;
        tracing::debug!(address = %config.address, "Mock: wake (no-op)");
        Ok(())
    }

    async fn wake_and_connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        // Wake is a no-op for the mock, so this is just `connect`.
        self.connect(vendor_config).await
    }

    fn start_pairing(&self, vendor_config: &toml::Value) -> Result<PairingSession> {
        // Validate the config early so bad input surfaces before the
        // session starts.
        let _config = parse_config(vendor_config)?;

        let (session, mut command_rx, state_tx) = PairingSession::create();

        // Spawn a tiny task that completes pairing instantly unless
        // the caller cancels. Matches the shape of LG's pairing flow
        // without needing a real handshake.
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                    let _ = state_tx.send(PairingState::complete(
                        Credentials::with_type("mock-key", "client_key"),
                    ));
                }
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(PairingCommand::Cancel) | None => {
                            let _ = state_tx.send(PairingState::failed(
                                tisvlay_core::PairingError::cancelled(),
                            ));
                        }
                        Some(PairingCommand::SubmitInput(_)) => {
                            // Mock doesn't use PIN input; succeed anyway.
                            let _ = state_tx.send(PairingState::complete(
                                Credentials::with_type("mock-key", "client_key"),
                            ));
                        }
                    }
                }
            }
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
