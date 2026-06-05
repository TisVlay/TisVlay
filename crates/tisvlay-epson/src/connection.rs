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
        // The feature surface the projector exposes, including WOL (a
        // feature bit, part of STOCK). Only the IPV4 / IPV6
        // transport-family bits stay backend-level — they're not
        // per-connection — matching LG's connection, which returns
        // STOCK (WoL included) without the IP-family bits.
        Capabilities::POWER
            | Capabilities::SCREEN
            | Capabilities::INPUT
            | Capabilities::BACKLIGHT
            | Capabilities::WOL
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
    // The connection's live capabilities drive the D-Bus
    // `Tv1.Capabilities` property, so WOL must be present (a feature
    // bit) while the IP-family transport bits stay backend-only.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn connection_capabilities_include_wol() {
        let server = FakeEpsonServer::start().await;
        let conn = connect(&server).await;
        let caps = conn.capabilities();

        assert_eq!(
            caps,
            Capabilities::POWER
                | Capabilities::SCREEN
                | Capabilities::INPUT
                | Capabilities::BACKLIGHT
                | Capabilities::WOL,
        );
        assert!(caps.contains(Capabilities::WOL), "wol must be advertised");
        // Transport-family bits are backend-level, not per-connection.
        assert!(!caps.contains(Capabilities::IPV4));
        assert!(!caps.contains(Capabilities::IPV6));
        // No volume/mute on a speakerless projector.
        assert!(!caps.contains(Capabilities::VOLUME));
        assert!(!caps.contains(Capabilities::MUTE));
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

    // A socket failure *during* a command (the server drops after
    // reading the command but before replying) surfaces as a
    // structured connection-class error and is recovered by the same
    // reconnect-and-retry path — distinct from the graceful between-
    // commands FIN above.
    #[tokio::test]
    async fn reconnects_after_mid_command_socket_failure() {
        let server = FakeEpsonServer::start().await;
        server.set_power_code("01");
        let mut conn = connect(&server).await;

        // First command succeeds on the live socket.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);

        // The next command is aborted mid-exchange; the client must
        // reconnect transparently and return the value on retry.
        server.abort_next_command();
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);
        assert!(conn.is_connected());
    }

    // -----------------------------------------------------------------------
    // get_state never sinks the snapshot on a sub-query failure: a
    // projector in standby rejects SOURCE? / LUMLEVEL? / MUTE?, but the
    // power state still comes back.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_state_in_standby_returns_power_only() {
        let server = FakeEpsonServer::start().await;
        // Standby: the fake answers ERR for source/brightness/mute.
        server.set_power_code("04");
        let mut conn = connect(&server).await;

        let state = conn
            .get_state()
            .await
            .expect("standby snapshot must not error");
        assert_eq!(state.power_state, PowerState::Standby);
        assert!(!state.is_on);
        // The gated sub-queries weren't issued, so the fields are empty
        // rather than the snapshot being dropped.
        assert_eq!(state.current_input, None);
        assert_eq!(state.backlight, None);
        // The advertised input list is still populated (it's static).
        assert_eq!(state.inputs.len(), 6);
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

    // -----------------------------------------------------------------------
    // power_off drops the transport on every outcome — including when
    // the projector never cleanly acks because it's powering down. The
    // socket-drop case represents the broader "any non-ERR outcome"
    // contract: a command timeout resolves through the same
    // `Err(_) => Ok(())` arm.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn power_off_drops_transport_even_without_clean_ack() {
        let server = FakeEpsonServer::start().await;
        server.set_power_code("01");
        let mut conn = connect(&server).await;
        assert!(conn.is_connected());

        // The projector drops the socket mid-`PWR OFF` instead of
        // acking — the realistic powering-down path.
        server.abort_next_command();
        // Still reported as success (the projector is going down), and
        // the transport is released regardless.
        conn.power_off()
            .await
            .expect("power_off treats a dropped ack as success");
        assert!(
            !conn.is_connected(),
            "the transport must be dropped on every power_off outcome",
        );
    }

    // -----------------------------------------------------------------------
    // A latched PowerIntent is cleared once a definitively-steady code
    // is read, so a much-later indistinct-busy reading isn't colored by
    // a stale direction.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn steady_reading_clears_stale_intent() {
        let server = FakeEpsonServer::start().await;
        server.set_power_code("01");
        let mut conn = connect(&server).await;

        // power_on latches PoweringOn.
        conn.client.power_on().await.unwrap();
        // A steady On reading clears the intent.
        assert_eq!(conn.get_power_state().await.unwrap(), PowerState::Active);

        // A later indistinct-busy reading must now read as Unknown (no
        // direction), not WarmingUp from the stale intent.
        server.set_power_code("99");
        assert_eq!(
            conn.get_power_state().await.unwrap(),
            PowerState::Unknown,
            "intent was cleared by the steady On reading, so a busy code is directionless",
        );
    }
}
