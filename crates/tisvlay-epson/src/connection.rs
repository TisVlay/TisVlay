//! `EpsonConnection` — a live control handle for one projector.
//!
//! Wraps an [`EpsonClient`] and forwards the [`TvConnection`] feature
//! methods the projector supports onto ESC/VP21 command exchanges.
//! Power, screen (A/V mute), input, and backlight (LUMLEVEL) are
//! implemented; volume / mute / state-subscription are left at their
//! `NotSupported` defaults — the projector is speakerless and ESC/VP21
//! has no push channel, so the daemon polls.

use async_trait::async_trait;
use tisvlay_core::{Capabilities, InputInfo, PowerState, Result, TvConnection, TvState};

use crate::client::EpsonClient;

/// A connection to an Epson projector.
#[derive(Debug)]
pub struct EpsonConnection {
    client: EpsonClient,
}

impl EpsonConnection {
    /// Wrap a connected [`EpsonClient`] as a [`TvConnection`].
    pub fn new(client: EpsonClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TvConnection for EpsonConnection {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    async fn get_state(&mut self) -> Result<TvState> {
        self.client.get_state().await
    }

    async fn close(&mut self) -> Result<()> {
        self.client.close().await
    }

    fn capabilities(&self) -> Capabilities {
        // Mirrors EpsonBackend::capabilities (minus the transport-family
        // bits, which aren't per-connection): the feature surface the
        // projector actually exposes.
        Capabilities::POWER | Capabilities::SCREEN | Capabilities::INPUT | Capabilities::BACKLIGHT
    }

    // ------------------------------------------------------------------
    // Power
    // ------------------------------------------------------------------

    async fn power_off(&mut self) -> Result<()> {
        self.client.power_off().await
    }

    async fn get_power_state(&mut self) -> Result<PowerState> {
        self.client.get_power_state().await
    }

    // ------------------------------------------------------------------
    // Screen (A/V mute)
    // ------------------------------------------------------------------

    async fn screen_on(&mut self) -> Result<()> {
        self.client.set_screen(true).await
    }

    async fn screen_off(&mut self) -> Result<()> {
        self.client.set_screen(false).await
    }

    async fn is_screen_on(&mut self) -> Result<bool> {
        self.client.is_screen_on().await
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        self.client.switch_input(input_id).await
    }

    async fn get_current_input(&mut self) -> Result<Option<String>> {
        self.client.current_input().await
    }

    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        Ok(self.client.list_inputs())
    }

    // ------------------------------------------------------------------
    // Backlight (LUMLEVEL)
    // ------------------------------------------------------------------

    async fn set_backlight(&mut self, level: u8) -> Result<()> {
        self.client.set_backlight(level).await
    }

    async fn get_backlight(&mut self) -> Result<u8> {
        self.client.get_backlight().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::FakeEpsonServer;
    use std::time::Duration;

    /// Build an `EpsonConnection` pointed at a fake server.
    async fn connect(handle: &crate::test_server::FakeEpsonHandle) -> EpsonConnection {
        let client = EpsonClient::connect(handle.host(), handle.port())
            .await
            .expect("connect to fake server");
        EpsonConnection::new(client)
    }

    // -----------------------------------------------------------------------
    // Headline: power-on through WarmingUp then Active.
    //
    // The server returns the warming code `02` for two `PWR?` reads
    // after a `PWR ON`, then `01`. The connection must report
    // `WarmingUp` while warming and `Active` once on.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn power_on_reports_warming_then_active() {
        let server = FakeEpsonServer::start().await;
        // Start in standby, then script two warming reads before on.
        server.set_power_code("04");
        server.script_warmup(2, "02");

        let mut conn = connect(&server).await;

        // Drive the projector on (the connector's wake path does this;
        // here we exercise the underlying client directly).
        conn.client.power_on().await.unwrap();

        // First two polls see the warming transient.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::WarmingUp);
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::WarmingUp);
        // Third poll: the projector is on (and unmuted) → Active.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);
    }

    // -----------------------------------------------------------------------
    // Screen via A/V mute.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn screen_off_on_flips_mute_and_power_state() {
        let server = FakeEpsonServer::start().await;
        server.set_power_code("01");
        // The fake server binds loopback.
        assert!(server.addr().ip().is_loopback());
        let mut conn = connect(&server).await;

        // A directly-injected mute is reflected by is_screen_on.
        server.set_mute(true);
        assert!(!conn.is_screen_on().await.unwrap());
        server.set_mute(false);

        // Starts on + unmuted → screen on, Active.
        assert!(conn.is_screen_on().await.unwrap());
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);

        // Blank the image.
        conn.screen_off().await.unwrap();
        assert!(!conn.is_screen_on().await.unwrap());
        // On + A/V-muted derives to ScreenOff.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::ScreenOff);

        // Unblank.
        conn.screen_on().await.unwrap();
        assert!(conn.is_screen_on().await.unwrap());
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);
    }

    // -----------------------------------------------------------------------
    // Input switch + read-back.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn input_switch_and_readback() {
        let server = FakeEpsonServer::start().await;
        server.set_source("30"); // HDMI_1
        let mut conn = connect(&server).await;

        assert_eq!(
            conn.get_current_input().await.unwrap().as_deref(),
            Some("HDMI_1")
        );

        conn.switch_input("HDMI_2").await.unwrap();
        assert_eq!(
            conn.get_current_input().await.unwrap().as_deref(),
            Some("HDMI_2")
        );

        // The advertised input list is the known set.
        let inputs = conn.list_inputs().await.unwrap();
        assert_eq!(inputs.len(), 6);
        assert!(inputs.iter().any(|i| i.id == "HDMI_1"));

        // An unknown input is rejected before any wire traffic.
        assert!(conn.switch_input("HDMI_9").await.is_err());
    }

    // -----------------------------------------------------------------------
    // Backlight via LUMLEVEL.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn backlight_set_get_via_lumlevel() {
        let server = FakeEpsonServer::start().await;
        let mut conn = connect(&server).await;

        conn.set_backlight(72).await.unwrap();
        assert_eq!(conn.get_backlight().await.unwrap(), 72);

        // Set is clamped to 0..=100.
        conn.set_backlight(200).await.unwrap();
        assert_eq!(conn.get_backlight().await.unwrap(), 100);
    }

    // -----------------------------------------------------------------------
    // get_state composes the four queries.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_state_snapshot() {
        let server = FakeEpsonServer::start().await;
        server.set_power_code("01");
        server.set_source("A0"); // HDMI_2
        let mut conn = connect(&server).await;
        conn.set_backlight(40).await.unwrap();

        let state = conn.get_state().await.unwrap();
        assert!(state.is_on);
        assert!(state.is_screen_on);
        assert_eq!(state.current_input.as_deref(), Some("HDMI_2"));
        assert_eq!(state.backlight, Some(40));
        assert_eq!(state.inputs.len(), 6);
    }

    // -----------------------------------------------------------------------
    // Reconnect after the projector drops the socket (standby behavior).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn reconnects_after_socket_drop() {
        let server = FakeEpsonServer::start().await;
        server.set_power_code("01");
        let mut conn = connect(&server).await;

        // First command succeeds.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);

        // Server drops the active connection (as on standby).
        server.drop_connection();
        // Give the accept loop a moment to tear down and re-arm.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // The next command detects the stale socket, reconnects to the
        // still-listening server, and succeeds.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);
        assert!(conn.is_connected());
    }

    // -----------------------------------------------------------------------
    // Intent-based warmup/cooldown disambiguation.
    //
    // The server answers the same indistinct busy code (`99`, which the
    // codec classifies as Unknown) for both a power-on and a power-off
    // transition. The client's last-power-intent picks the direction.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn intent_disambiguates_warmup_vs_cooldown() {
        // Power-on direction → WarmingUp.
        {
            let server = FakeEpsonServer::start().await;
            server.set_power_code("04");
            // Two indistinct-busy reads before the steady code; the
            // server snaps power_code to 01 once warmup completes.
            server.script_warmup(2, "99");
            let mut conn = connect(&server).await;
            conn.client.power_on().await.unwrap();
            assert_eq!(
                conn.get_power_state().await.unwrap(),
                PowerState::WarmingUp,
                "indistinct busy + PoweringOn intent → WarmingUp",
            );
        }

        // Power-off direction → CoolingDown.
        {
            let server = FakeEpsonServer::start().await;
            server.set_power_code("01");
            server.script_cooldown(2, "99");
            let mut conn = connect(&server).await;
            // power_off records the PoweringOff intent and drops the
            // socket; the next poll reconnects. The cooldown progression
            // survives on the shared server state, and the intent
            // survives on the client across the reconnect.
            conn.client.power_off().await.unwrap();
            assert_eq!(
                conn.get_power_state().await.unwrap(),
                PowerState::CoolingDown,
                "indistinct busy + PoweringOff intent → CoolingDown",
            );
        }
    }
}
