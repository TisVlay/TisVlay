//! LG WebOS connection factory — the [`TvConnector`] implementation.
//!
//! [`LgConnector`] turns a vendor config into a live
//! [`LgConnection`](crate::connection::LgConnection): a single-attempt
//! `connect`, the WoL `wake_and_connect` retry loop, the pairing
//! handshake, and the persisted-config schema.

use crate::client::WebOsClient;
use crate::config::LgVendorConfig;
use crate::connection::LgConnection;
use async_trait::async_trait;
use std::time::Duration;
use tisvlay_core::{
    Credentials, Error, MacAddress, PairingCommand, PairingSession, PairingState, Result,
    TvConnection, TvConnector,
};
use tracing::{debug, info, warn};

/// Maximum number of connection attempts after sending WoL.
const WAKE_CONNECT_MAX_ATTEMPTS: u32 = 60;

/// Delay between connection retry attempts during wake-and-connect.
/// Each attempt is cheap (~50ms TCP connect + close frame), so we
/// poll aggressively to find the earliest moment the TV is ready.
const WAKE_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Helper to deserialize LgVendorConfig from a toml::Value.
fn parse_config(vendor_config: &toml::Value) -> Result<LgVendorConfig> {
    vendor_config
        .clone()
        .try_into()
        .map_err(|e| Error::backend("lg", format!("Invalid vendor config: {}", e)))
}

/// Factory for creating LG WebOS connections.
#[derive(Debug)]
pub struct LgConnector;

impl LgConnector {
    /// Create a new LG connector.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LgConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TvConnector for LgConnector {
    fn transport(&self) -> tisvlay_core::Transport {
        tisvlay_core::Transport::Network
    }

    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = parse_config(vendor_config)?;

        debug!(
            ip = %config.ip,
            has_key = config.client_key.is_some(),
            scheme = %config.scheme,
            "Connecting to LG TV"
        );

        // Single attempt — fail-fast contract. Stale-session
        // takeover and other "TV not ready yet" cases are handled
        // by `wake_and_connect`'s outer retry loop, which is the
        // only retry layer.
        let mut client =
            WebOsClient::connect_with_scheme(&config.ip, config.scheme, config.client_key.clone())
                .await?;

        // Best-effort identity fetch right after connect. Lets the
        // daemon fill the identity payload's hardware fields
        // (`meta/identity`) on the first registration, before any
        // user interaction.
        let hardware_id = match client.get_hardware_id().await {
            Ok(id) => {
                debug!(ip = %config.ip, ?id, "fetched LG hardware identity");
                Some(id)
            }
            Err(e) => {
                debug!(ip = %config.ip, error = %e, "getSystemInfo failed");
                None
            }
        };
        Ok(Box::new(LgConnection::new(client, config, hardware_id)))
    }

    fn supports_wake(&self, vendor_config: &toml::Value) -> bool {
        parse_config(vendor_config)
            .map(|c| c.mac.is_some())
            .unwrap_or(false)
    }

    async fn wake(&self, vendor_config: &toml::Value) -> Result<()> {
        let config = parse_config(vendor_config)?;
        if let Some(ref mac) = config.mac {
            let mac_addr = MacAddress::parse(mac)?;
            tisvlay_core::wol::wake_with_retry(&mac_addr, 8).await?;
            debug!(ip = %config.ip, mac = %mac, "WoL packet sent");
        }
        Ok(())
    }

    async fn wake_and_connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        self.wake(vendor_config).await?;

        let config = parse_config(vendor_config)?;
        let mut last_err = None;

        for attempt in 1..=WAKE_CONNECT_MAX_ATTEMPTS {
            // Resend WoL before retrying — earlier packets may have been
            // lost if the desktop network wasn't up after system resume.
            if attempt > 1
                && let Some(ref mac) = config.mac
                && let Ok(mac_addr) = MacAddress::parse(mac)
            {
                tisvlay_core::wol::wake_best_effort(&mac_addr);
            }

            match self.connect(vendor_config).await {
                Ok(conn) => {
                    info!(ip = %config.ip, attempt, "LG TV online after wake");
                    return Ok(conn);
                }
                Err(e) => {
                    debug!(ip = %config.ip, attempt, error = %e, "LG TV not ready yet");
                    last_err = Some(e);
                    tokio::time::sleep(WAKE_CONNECT_RETRY_INTERVAL).await;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| Error::Backend {
            vendor: "lg".into(),
            message: format!("LG TV at {} did not come online after wake", config.ip),
            source: None,
        }))
    }

    fn start_pairing(&self, vendor_config: &toml::Value) -> Result<PairingSession> {
        let config = parse_config(vendor_config)?;
        let ip = config.ip.clone();

        let (session, mut command_rx, state_tx) = PairingSession::create();

        // Spawn the pairing task
        tokio::spawn(async move {
            // Update state to awaiting TV prompt
            let _ = state_tx.send(PairingState::awaiting_tv_prompt(
                "Please accept the pairing prompt on your LG TV",
            ));

            // Race the pairing operation against cancel commands so that
            // PairingCommand::Cancel actually interrupts the WebSocket
            // connect/handshake phase.
            tokio::select! {
                result = WebOsClient::connect(&ip, None) => {
                    match result {
                        Ok(client) => {
                            if let Some(key) = client.client_key() {
                                let scheme = client.scheme().unwrap_or_default();
                                info!(ip = %ip, %scheme, "LG TV pairing successful");
                                // Capture which scheme connected so
                                // subsequent reconnects skip the
                                // wss→ws fallback (see
                                // `LgVendorConfig.scheme`).
                                let creds = Credentials::with_type(key, "client_key")
                                    .with_extra("scheme", scheme.as_str());
                                let _ = state_tx.send(PairingState::complete(creds));
                            } else {
                                warn!(ip = %ip, "LG TV connected but no client key received");
                                let _ = state_tx.send(PairingState::failed(
                                    tisvlay_core::PairingError::connection_failed(
                                        "Connected but no client key received",
                                    ),
                                ));
                            }
                        }
                        Err(e) => {
                            warn!(ip = %ip, error = %e, "LG TV pairing failed");
                            let state = match &e {
                                Error::PairingRejected => {
                                    PairingState::failed(tisvlay_core::PairingError::rejected())
                                }
                                Error::PairingTimeout => {
                                    PairingState::failed(tisvlay_core::PairingError::timeout())
                                }
                                _ => PairingState::failed(
                                    tisvlay_core::PairingError::connection_failed(e.to_string()),
                                ),
                            };
                            let _ = state_tx.send(state);
                        }
                    }
                }
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(PairingCommand::Cancel) | None => {
                            info!(ip = %ip, "LG TV pairing cancelled");
                            let _ = state_tx.send(PairingState::failed(
                                tisvlay_core::PairingError::cancelled(),
                            ));
                        }
                        Some(PairingCommand::SubmitInput(_)) => {
                            // LG doesn't use PIN/password input; continue waiting
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
        credentials: &Credentials,
    ) -> Result<toml::Value> {
        let mut config = parse_config(pairing_params)?;
        config.client_key = Some(credentials.data.clone());
        // Persist the working WebSocket scheme captured during the
        // pairing handshake — `connect()` reads this on reconnects
        // to skip the wss→ws fallback. Pairing always populates
        // this; reject if it's missing rather than silently
        // defaulting (would mask a bug in the pairing flow).
        let scheme = credentials.extra.get("scheme").ok_or_else(|| {
            Error::backend("lg", "pairing credentials missing `scheme` — internal bug")
        })?;
        config.scheme = scheme.parse()?;
        toml::Value::try_from(&config)
            .map_err(|e| Error::backend("lg", format!("Failed to serialize config: {}", e)))
    }

    fn config_schema(&self) -> Vec<tisvlay_core::PairingField> {
        use tisvlay_core::{FieldVisibility, PairingField, PairingFieldType};
        // Full persisted-config shape. Visibility tags drive
        // projection: wizard sees `UserInput` only; INFO pane
        // sees `UserInput + Diagnostic`; nothing ever sees
        // `Secret`. Adding a new field here automatically reaches
        // every consumer (or doesn't, depending on its tag) — no
        // separate `display_info` to keep in sync.
        vec![
            PairingField {
                name: "ip".into(),
                label: "IP".into(),
                helper: "LAN address of the TV's WebOS interface.".into(),
                field_type: PairingFieldType::Ip,
                required: true,
                placeholder: "192.168.1.100".into(),
                // SSDP discovery populates `vendor_data["ip"]`;
                // this autofill key matches.
                autofill: "discovery.ip".into(),
                ..Default::default()
            },
            PairingField {
                name: "mac".into(),
                label: "MAC".into(),
                helper: "Required for Wake-on-LAN. Leave blank to read it \
                         from the TV after pairing."
                    .into(),
                field_type: PairingFieldType::Mac,
                // Optional at the schema level; the wizard surfaces
                // a warning when empty but lets the user proceed
                // since some setups manage power externally.
                required: false,
                placeholder: "AA:BB:CC:DD:EE:FF".into(),
                // Daemon ARP-resolves this from the kernel's neighbor
                // table after SSDP yields the IP and stuffs it into
                // `vendor_data["mac"]`.
                autofill: "discovery.mac".into(),
                ..Default::default()
            },
            PairingField {
                name: "client_key".into(),
                label: "Client key".into(),
                helper: "Authentication token issued by the TV during \
                         the pairing handshake."
                    .into(),
                field_type: PairingFieldType::Password,
                // Backend-discovered (`build_config_from_pairing`
                // writes it). Never displayed; never editable —
                // re-pair to refresh.
                visibility: FieldVisibility::Secret,
                ..Default::default()
            },
            PairingField {
                name: "scheme".into(),
                label: "WebSocket".into(),
                helper: "Captured at pair time — `wss` (port 3001 + TLS) \
                         or `ws` (port 3000, plain). Re-pair to refresh."
                    .into(),
                field_type: PairingFieldType::Select,
                options: vec!["wss".into(), "ws".into()],
                // Useful for diagnosis ("which port is the daemon
                // talking to?") but not user-editable.
                visibility: FieldVisibility::Diagnostic,
                ..Default::default()
            },
        ]
    }

    async fn probe_arp_cache(&self, ip: std::net::IpAddr) -> Result<()> {
        // Single-byte UDP datagram to the SSAP wss port. The kernel
        // resolves ARP (or NDP, for v6) before sending; that's the
        // side effect we want. The TV's UDP stack discards the
        // payload silently — no TLS handshake side-effect, no
        // inbound port needed, no raw sockets / CAP_NET_RAW.
        //
        // Bind family matches the destination so the socket has a
        // routable local address; the kernel's routing table then
        // picks the correct outbound NIC + source IP. Explicit
        // source-IP binding via netlink route lookup would only
        // matter for VPN-policy-routing edge cases — out of scope
        // until a real user hits it (see
        // `docs/ideas/wol-raw-socket.md` for the broader topology-
        // flexibility story).
        let bind_addr: std::net::SocketAddr = if ip.is_ipv6() {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
        };
        let socket = tokio::net::UdpSocket::bind(bind_addr).await?;
        let _ = socket
            .send_to(&[0u8], (ip, crate::endpoints::WSS_PORT))
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // build_config_from_pairing
    // -----------------------------------------------------------------------

    #[test]
    fn build_config_from_pairing_stores_key() {
        let connector = LgConnector::new();
        let params = toml::Value::try_from(LgVendorConfig::new("192.168.1.100")).unwrap();
        let creds =
            Credentials::with_type("my-client-key", "client_key").with_extra("scheme", "wss");
        let result = connector
            .build_config_from_pairing(&params, &creds)
            .unwrap();
        let config: LgVendorConfig = result.try_into().unwrap();
        assert_eq!(config.ip, "192.168.1.100");
        assert_eq!(config.client_key.as_deref(), Some("my-client-key"));
        assert_eq!(config.scheme, crate::config::WebOsScheme::Wss);
    }

    #[test]
    fn build_config_preserves_mac() {
        let connector = LgConnector::new();
        let params = toml::Value::try_from(LgVendorConfig {
            mac: Some("AA:BB:CC:DD:EE:FF".into()),
            ..LgVendorConfig::new("10.0.0.1")
        })
        .unwrap();
        let creds = Credentials::with_type("key", "client_key").with_extra("scheme", "ws");
        let result = connector
            .build_config_from_pairing(&params, &creds)
            .unwrap();
        let config: LgVendorConfig = result.try_into().unwrap();
        assert_eq!(config.mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(config.client_key.as_deref(), Some("key"));
        assert_eq!(config.scheme, crate::config::WebOsScheme::Ws);
    }

    // -----------------------------------------------------------------------
    // start_pairing accepts already-credentialed vendor_config
    //
    // Re-pair calls `start_pairing` against a TV's existing
    // vendor_config — which may include a `client_key` from a prior
    // pair. Verify the LG backend doesn't error on that input. Note:
    // LG's implementation hardcodes `WebOsClient::connect(&ip, None)` (the
    // existing key is ignored), so every re-pair triggers a fresh TV prompt;
    // that's documented behavior, not a bug to fix here.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn start_pairing_accepts_credentialed_vendor_config() {
        let connector = LgConnector;

        // Construct a vendor_config that includes a pre-existing client_key
        // (i.e., what re-pair would supply). The fake IP means the spawned
        // WebSocket task will fail asynchronously, but `start_pairing` itself
        // returns immediately with a session — that's what we're verifying.
        let mut table = toml::value::Table::new();
        table.insert("ip".into(), toml::Value::String("192.0.2.99".into()));
        table.insert(
            "client_key".into(),
            toml::Value::String("preexisting-key".into()),
        );
        table.insert(
            "mac".into(),
            toml::Value::String("AA:BB:CC:DD:EE:FF".into()),
        );
        let vendor_config = toml::Value::Table(table);

        let session1 = connector.start_pairing(&vendor_config);
        assert!(
            session1.is_ok(),
            "first start_pairing with credentialed config should return Ok; got {:?}",
            session1.as_ref().err(),
        );
        drop(session1);

        // Second call with the same input must also return Ok — re-pair
        // may be invoked multiple times against the same vendor_config.
        let session2 = connector.start_pairing(&vendor_config);
        assert!(
            session2.is_ok(),
            "second start_pairing with the same credentialed config should return Ok; got {:?}",
            session2.as_ref().err(),
        );
    }
}
