//! `EpsonConnector` — factory and lifecycle owner for the Epson
//! backend.
//!
//! Builds [`EpsonConnection`] handles from a vendor config, owns the
//! wake path (WoL magic packet plus the warmup poll to
//! [`PowerState::Active`]), and drives the trivial no-auth pairing
//! flow (a connectivity check — ESC/VP21 over TCP is unauthenticated).

use async_trait::async_trait;
use std::time::Duration;
use tisvlay_core::{
    Credentials, Error, MacAddress, PairingCommand, PairingError, PairingSession, PairingState,
    PowerState, Result, Transport, TvConnection, TvConnector,
};
use tracing::{debug, info, warn};

use crate::client::EpsonClient;
use crate::config::{self, EpsonVendorConfig};
use crate::connection::EpsonConnection;

/// Maximum number of connect attempts after sending WoL.
const WAKE_CONNECT_MAX_ATTEMPTS: u32 = 60;

/// Overall wall-clock budget for the wake-and-connect retry loop. The
/// attempt cap alone is not a time bound — each attempt can hang up to
/// the per-attempt connect timeout (10 s), so 60 attempts could span
/// ~10 min on a handshake stall. This deadline caps the total at ~90 s
/// (comfortably past a cold-NIC recovery) regardless of how long
/// individual attempts take.
const WAKE_CONNECT_TOTAL_BUDGET: Duration = Duration::from_secs(90);

/// Delay between connect retries during wake-and-connect. The
/// projector's NIC may still be coming up after deep standby, so the
/// loop spans tens of seconds.
const WAKE_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(500);

/// Maximum number of WoL send retries while the local network recovers
/// (e.g. after system resume).
const WAKE_SEND_MAX_ATTEMPTS: u32 = 8;

/// Maximum time to poll `PWR?` through warmup before giving up. The
/// LS12000 warms in ~30–40 s; 60 s leaves headroom for a cold start.
const WARMUP_POLL_TIMEOUT: Duration = Duration::from_secs(60);

/// Interval between warmup `PWR?` polls.
const WARMUP_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum time to wait for a still-cooling projector to leave cooldown
/// before sending `PWR ON`. A projector powered off moments ago is in a
/// cooldown lockout where `PWR ON` is rejected; the LS12000 cools in a
/// comparable window to its warmup, so ~30 s covers a quick power-off
/// then power-on without making the common (already-standby) path wait.
const COOLDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Factory for creating Epson projector connections.
#[derive(Debug, Default)]
pub struct EpsonConnector;

impl EpsonConnector {
    /// Create a new Epson connector.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TvConnector for EpsonConnector {
    fn transport(&self) -> Transport {
        Transport::Network
    }

    fn survives_source_ip_change(&self) -> bool {
        // The ESC/VP21 session is a persistent TCP socket bound to a
        // 4-tuple — a source-IP change on NIC failover breaks it, so
        // the daemon must reconnect (matches LG's WebSocket).
        false
    }

    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = EpsonVendorConfig::from_toml(vendor_config)?;
        debug!(ip = %config.ip, port = config.port, "connecting to Epson projector");
        let client = EpsonClient::connect(config.ip.clone(), config.port).await?;
        Ok(Box::new(EpsonConnection::new(client)))
    }

    fn supports_wake(&self, vendor_config: &toml::Value) -> bool {
        EpsonVendorConfig::from_toml(vendor_config)
            .map(|c| c.mac.is_some())
            .unwrap_or(false)
    }

    async fn wake(&self, vendor_config: &toml::Value) -> Result<()> {
        let config = EpsonVendorConfig::from_toml(vendor_config)?;
        let Some(mac) = &config.mac else {
            // No MAC → nothing to send. Network `PWR ON` (for a
            // Communication-On standby) is driven by the connection's
            // power path, not here.
            return Ok(());
        };

        // Retry the magic packet while the local NIC recovers — the
        // first attempt can fail with "network unreachable" right after
        // system resume.
        let mac_addr = MacAddress::parse(mac)?;
        tisvlay_core::wol::wake_with_retry(&mac_addr, WAKE_SEND_MAX_ATTEMPTS).await?;
        debug!(ip = %config.ip, %mac, "WoL packet sent");
        Ok(())
    }

    async fn wake_and_connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>> {
        let config = EpsonVendorConfig::from_toml(vendor_config)?;

        // Send the magic packet if a MAC is configured. A connect
        // failure below resends it — early packets may be lost while
        // the desktop network is still recovering.
        self.wake(vendor_config).await?;

        let deadline = tokio::time::Instant::now() + WAKE_CONNECT_TOTAL_BUDGET;
        let mut client = None;
        let mut last_err = None;
        for attempt in 1..=WAKE_CONNECT_MAX_ATTEMPTS {
            // Resend the magic packet before retrying — earlier packets
            // may have been lost while the desktop network was still
            // recovering after system resume.
            if attempt > 1 {
                if let Some(mac) = &config.mac {
                    if let Ok(mac_addr) = MacAddress::parse(mac) {
                        tisvlay_core::wol::wake_best_effort(&mac_addr);
                    }
                }
            }
            match EpsonClient::connect(config.ip.clone(), config.port).await {
                Ok(c) => {
                    info!(ip = %config.ip, attempt, "Epson projector reachable after wake");
                    client = Some(c);
                    break;
                }
                Err(e) => {
                    debug!(ip = %config.ip, attempt, error = %e, "projector not ready yet");
                    last_err = Some(e);
                    // Stop once the overall budget is spent — a hung
                    // handshake on each attempt could otherwise stretch
                    // the attempt cap into minutes.
                    if tokio::time::Instant::now() >= deadline {
                        debug!(ip = %config.ip, attempt, "wake-connect budget exhausted");
                        break;
                    }
                    tokio::time::sleep(WAKE_CONNECT_RETRY_INTERVAL).await;
                }
            }
        }

        let mut client = client.ok_or_else(|| {
            last_err.unwrap_or_else(|| {
                Error::backend(
                    crate::VENDOR_ID,
                    format!("projector at {} did not come online after wake", config.ip),
                )
            })
        })?;

        // A projector re-powered shortly after a power-off is still in
        // its cooldown lockout, where `PWR ON` is rejected. Wait for it
        // to leave cooldown before driving it on, so a quick
        // power-off → power-on doesn't spuriously fail the warmup poll.
        // No-op on the common path (an already-standby or already-on
        // projector isn't cooling).
        wait_out_cooldown(&mut client, &config.ip).await?;

        // Drive the projector on and poll through warmup to Active so
        // downstream action chains (on_startup = power_on → switch_input)
        // don't fire while the laser is still warming and no usable image
        // is projected. A projector that was already on returns Active on
        // the first poll, so this is a fast path in the common case.
        client.power_on().await?;
        poll_until_active(&mut client, &config.ip).await?;

        Ok(Box::new(EpsonConnection::new(client)))
    }

    fn start_pairing(&self, vendor_config: &toml::Value) -> Result<PairingSession> {
        let config = EpsonVendorConfig::from_toml(vendor_config)?;
        let host = config.ip.clone();
        let port = config.port;

        let (session, mut command_rx, state_tx) = PairingSession::create();

        tokio::spawn(async move {
            // ESC/VP21 over TCP has no authentication — "pairing" is a
            // connectivity check. Show an awaiting prompt, then race the
            // handshake against a cancel command.
            let _ = state_tx.send(PairingState::awaiting_tv_prompt(
                "Checking the connection to your Epson projector...",
            ));

            tokio::select! {
                result = EpsonClient::connect(host.clone(), port) => {
                    match result {
                        Ok(mut client) => {
                            info!(ip = %host, "Epson projector reachable; pairing complete");
                            let _ = client.close().await;
                            // No credential to store — the empty key is a
                            // deliberate marker for the no-auth flow.
                            let _ = state_tx.send(PairingState::complete(Credentials::new("")));
                        }
                        Err(e) => {
                            warn!(ip = %host, error = %e, "Epson projector unreachable during pairing");
                            let _ = state_tx.send(PairingState::failed(
                                PairingError::connection_failed(e.to_string()),
                            ));
                        }
                    }
                }
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(PairingCommand::Cancel) | None => {
                            info!(ip = %host, "Epson pairing cancelled");
                            let _ = state_tx.send(PairingState::failed(PairingError::cancelled()));
                        }
                        Some(PairingCommand::SubmitInput(_)) => {
                            // No-auth flow takes no user input; keep waiting.
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
        // The credential is empty (no auth) — the persisted config is
        // just the normalized pairing params.
        EpsonVendorConfig::from_toml(pairing_params)?.to_toml()
    }

    fn config_schema(&self) -> Vec<tisvlay_core::PairingField> {
        config::config_schema()
    }

    async fn probe_arp_cache(&self, ip: std::net::IpAddr) -> Result<()> {
        // Best-effort neighbour-table priming for the pairing wizard's
        // `discovery.mac` autofill: a TCP connect to the ESC/VP21 port
        // makes the kernel resolve ARP (or NDP) before the SYN goes
        // out, leaving a fresh entry the daemon can then read. The
        // connect outcome is irrelevant — a refused or timed-out attempt
        // primes the table just as well — so this always returns Ok and
        // never blocks the caller for long.
        let addr = std::net::SocketAddr::new(ip, config::DEFAULT_PORT);
        let _ = tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr))
            .await;
        Ok(())
    }
}

/// Wait for a still-cooling projector to leave cooldown before a
/// `PWR ON`.
///
/// A `PWR ON` issued during the post-power-off cooldown lockout is
/// rejected, so when the freshly-connected projector reports
/// [`PowerState::CoolingDown`] this polls `PWR?` until it reaches a
/// non-cooling state (`Standby` once cooldown completes, or anything
/// else), bounded by [`COOLDOWN_WAIT_TIMEOUT`]. Any other connect-time
/// reading (`Active` / `ScreenOff` / `WarmingUp` / `Standby` /
/// `Unknown`) returns immediately — the projector isn't cooling, so
/// `PWR ON` will be accepted (or is already redundant).
///
/// A timeout here is not fatal: the caller proceeds to `PWR ON` +
/// [`poll_until_active`] regardless, which surfaces a genuine
/// won't-power-on as its own error.
async fn wait_out_cooldown(client: &mut EpsonClient, ip: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + COOLDOWN_WAIT_TIMEOUT;
    loop {
        if client.get_power_state().await? != PowerState::CoolingDown {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            debug!(
                %ip,
                "projector still cooling down after the cooldown-wait timeout; \
                 sending PWR ON anyway",
            );
            return Ok(());
        }
        debug!(%ip, "projector cooling down; waiting before PWR ON");
        tokio::time::sleep(WARMUP_POLL_INTERVAL).await;
    }
}

/// Poll `PWR?` until the projector reports [`PowerState::Active`],
/// tolerating the [`PowerState::WarmingUp`] and [`PowerState::CoolingDown`]
/// transients.
///
/// `CoolingDown` is treated as transient (keep polling) rather than a
/// hard error: the cooldown-wait in `wake_and_connect` already drains a
/// pre-existing cooldown before `PWR ON`, but a `PWR ON` racing the tail
/// of a cooldown can still read one stale cooling code, and it's bounded
/// by [`WARMUP_POLL_TIMEOUT`] either way. `Standby` stays the hard-error
/// case — once `PWR ON` has been sent, a steady standby reading means it
/// didn't take.
async fn poll_until_active(client: &mut EpsonClient, ip: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + WARMUP_POLL_TIMEOUT;
    loop {
        match client.get_power_state().await? {
            PowerState::Active | PowerState::ScreenOff => {
                // ScreenOff is on + A/V-muted: the projector is up; the
                // image being blanked is a separate, recoverable concern.
                return Ok(());
            }
            PowerState::WarmingUp | PowerState::CoolingDown | PowerState::Unknown => {
                // Still warming, draining a cooldown tail, or a transient
                // indistinct reading — keep polling.
            }
            PowerState::Standby => {
                return Err(Error::backend(
                    crate::VENDOR_ID,
                    format!(
                        "projector at {ip} reported Standby during warmup; PWR ON did not take"
                    ),
                ));
            }
            other => {
                // Any other substate isn't an expected warmup reading
                // for a projector; keep polling until the deadline.
                debug!(%ip, ?other, "unexpected state during warmup poll");
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(Error::backend(
                crate::VENDOR_ID,
                format!("projector at {ip} did not reach Active within the warmup timeout"),
            ));
        }
        tokio::time::sleep(WARMUP_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_value(ip: &str, mac: Option<&str>, port: u16) -> toml::Value {
        let mut cfg = EpsonVendorConfig::new(ip);
        cfg.mac = mac.map(str::to_string);
        cfg.port = port;
        cfg.to_toml().unwrap()
    }

    #[test]
    fn transport_is_network() {
        assert_eq!(EpsonConnector::new().transport(), Transport::Network);
    }

    #[test]
    fn does_not_survive_source_ip_change() {
        assert!(!EpsonConnector::new().survives_source_ip_change());
    }

    #[test]
    fn supports_wake_gated_on_mac() {
        let connector = EpsonConnector::new();
        assert!(connector.supports_wake(&config_value(
            "10.0.0.1",
            Some("AA:BB:CC:DD:EE:FF"),
            3629
        )));
        assert!(!connector.supports_wake(&config_value("10.0.0.1", None, 3629)));
    }

    #[tokio::test]
    async fn wake_without_mac_is_noop_ok() {
        let connector = EpsonConnector::new();
        // No MAC → wake is a no-op that succeeds without touching the
        // network.
        connector
            .wake(&config_value("10.0.0.1", None, 3629))
            .await
            .unwrap();
    }

    #[test]
    fn build_config_from_pairing_ignores_empty_credentials() {
        let connector = EpsonConnector::new();
        let params = config_value("192.168.1.50", Some("AA:BB:CC:DD:EE:FF"), 3629);
        let result = connector
            .build_config_from_pairing(&params, &Credentials::new(""))
            .unwrap();
        let config = EpsonVendorConfig::from_toml(&result).unwrap();
        assert_eq!(config.ip, "192.168.1.50");
        assert_eq!(config.mac.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
    }

    #[test]
    fn config_schema_delegates_to_config_module() {
        let schema = EpsonConnector::new().config_schema();
        assert_eq!(schema[0].name, "ip");
    }

    // -----------------------------------------------------------------------
    // Connector against the fake server.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn connect_returns_live_connection() {
        let server = crate::test_server::FakeEpsonServer::start().await;
        let connector = EpsonConnector::new();
        let config = config_value(&server.host(), None, server.port());

        let mut conn = connector.connect(&config).await.unwrap();
        assert!(conn.is_connected());
        assert_eq!(conn.vendor_id(), "epson");
        // A live query round-trips through the real transport.
        assert_eq!(
            conn.get_power_state().await.unwrap(),
            PowerState::Active,
            "default fake-server power code is 01 (on, unmuted) → Active",
        );
    }

    #[tokio::test]
    async fn connect_to_dead_address_fails_fast() {
        let connector = EpsonConnector::new();
        // Port 1 on loopback refuses fast.
        let config = config_value("127.0.0.1", None, 1);
        assert!(connector.connect(&config).await.is_err());
    }

    #[tokio::test]
    async fn start_pairing_completes_with_empty_credentials() {
        let server = crate::test_server::FakeEpsonServer::start().await;
        let connector = EpsonConnector::new();
        let config = config_value(&server.host(), None, server.port());

        let mut session = connector.start_pairing(&config).unwrap();
        let creds = tokio::time::timeout(Duration::from_secs(5), session.wait_for_completion())
            .await
            .expect("pairing should resolve within 5s")
            .expect("pairing should succeed against the fake server");
        // No-auth flow: the credential is empty.
        assert_eq!(creds.data, "");
    }

    #[tokio::test]
    async fn start_pairing_fails_when_unreachable() {
        let connector = EpsonConnector::new();
        let config = config_value("127.0.0.1", None, 1);

        let mut session = connector.start_pairing(&config).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(12), session.wait_for_completion())
            .await
            .expect("pairing should resolve within the connect timeout");
        assert!(result.is_err(), "unreachable projector → pairing fails");
    }

    // A quick power-off → power-on lands on a still-cooling projector.
    // `wake_and_connect` must wait out the cooldown before `PWR ON`
    // (which the firmware rejects mid-cooldown) and ride the subsequent
    // warmup to Active — not hard-error on the cooling reading. Script
    // the full cooling → standby → warming → on progression and assert
    // it succeeds.
    #[tokio::test]
    async fn wake_and_connect_rides_out_cooldown_then_warmup() {
        let server = crate::test_server::FakeEpsonServer::start().await;
        // Two cooling reads, then standby; the `PWR ON` that follows
        // arms a two-read warmup, then on. No MAC → wake() is a no-op,
        // so the connect loop reaches the live server on the first try.
        server.set_power_code("03");
        server.script_cooldown(2, "03");
        server.script_warmup_after_power_on(2, "02");

        let connector = EpsonConnector::new();
        let config = config_value(&server.host(), None, server.port());

        let conn = connector
            .wake_and_connect(&config)
            .await
            .expect("wake_and_connect must ride out cooldown + warmup without erroring");
        assert!(conn.is_connected());
    }

    // The wake-and-connect loop is bounded by an overall wall-clock
    // budget, not just the attempt cap — otherwise a per-attempt
    // handshake hang could stretch 60 attempts × the 10 s connect
    // timeout into ~10 min. Pin that the budget is meaningfully tighter
    // than that naive worst case so the deadline actually constrains
    // the loop.
    #[test]
    fn wake_connect_budget_is_tighter_than_attempt_cap_worst_case() {
        // Per-attempt connect timeout is 10 s (transport's CONNECT_TIMEOUT).
        let naive_worst_case = WAKE_CONNECT_MAX_ATTEMPTS * 10;
        assert!(
            WAKE_CONNECT_TOTAL_BUDGET.as_secs() < naive_worst_case as u64,
            "the total budget must bound the loop below the attempt-cap worst case",
        );
        // And it leaves headroom past a cold-NIC recovery (tens of secs).
        assert!(WAKE_CONNECT_TOTAL_BUDGET >= Duration::from_secs(60));
    }

    // `probe_arp_cache` primes the kernel neighbour table for the
    // wizard's `discovery.mac` autofill. It's best-effort: an
    // unreachable target still returns Ok (the connect attempt itself
    // is the priming side effect, outcome irrelevant).
    #[tokio::test]
    async fn probe_arp_cache_is_ok_for_unreachable_ip() {
        let connector = EpsonConnector::new();
        // Loopback port-1 refuses fast; the connect resolves quickly
        // and the probe swallows the result.
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        connector
            .probe_arp_cache(ip)
            .await
            .expect("probe is best-effort and returns Ok regardless of reachability");
    }
}
