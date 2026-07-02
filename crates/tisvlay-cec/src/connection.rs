//! `CecConnection` — a live CEC control handle for one TV.
//!
//! Surface: power (report state, turn off — power-on is the connector's
//! wake path, since the bus reaches the TV even in standby) and
//! active-source input switching (`Set Stream Path`). Everything else
//! stays at the trait's `NotSupported` default.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use linux_cec::message::Message;
use linux_cec::operand::PowerStatus;
use linux_cec::{LogicalAddress, PhysicalAddress};
use tisvlay_core::{Capabilities, Error, InputInfo, PowerState, Result, TvConnection, TvState};
use tokio::sync::mpsc;

use crate::link::{CecLink, CecLinkOpener};

/// A transient monitor rx error backs off this long before retrying; the
/// loop gives up only after this many *consecutive* errors — separating a
/// momentary bus glitch (recover) from a wedged adapter (stop).
const MONITOR_MAX_ERRORS: u32 = 5;
const MONITOR_ERROR_BACKOFF: Duration = Duration::from_millis(500);

/// A live CEC control handle for one TV.
#[derive(Debug)]
pub struct CecConnection {
    link: Box<dyn CecLink>,
    connected: bool,
    /// Retained so `subscribe_state` can open a second, follower-mode
    /// handle on the same device for the push-state receive loop.
    opener: Arc<dyn CecLinkOpener>,
    device: String,
    /// The `subscribe_state` monitor task. Aborted on drop so the state
    /// channel closes when the connection goes away — that closure is what
    /// drives the daemon's subscription-end `ConnectionChanged(false)`
    /// (e.g. on power-off). Without this the detached task would keep the
    /// channel open and the TV would read `Connected` forever.
    monitor_task: Option<tokio::task::JoinHandle<()>>,
}

impl CecConnection {
    pub(crate) fn new(
        link: Box<dyn CecLink>,
        opener: Arc<dyn CecLinkOpener>,
        device: String,
    ) -> Self {
        Self {
            link,
            connected: true,
            opener,
            device,
            monitor_task: None,
        }
    }
}

impl Drop for CecConnection {
    fn drop(&mut self) {
        if let Some(task) = self.monitor_task.take() {
            task.abort();
        }
    }
}

/// Extract a [`PowerState`] from an unsolicited inbound CEC message, if it
/// carries one. `<Report Power Status>` is authoritative; `<Standby>` and
/// the source-active / One-Touch-Play messages are what a TV broadcasts
/// when its power changes on its own (e.g. via its own remote).
fn power_from_message(message: &Message) -> Option<PowerState> {
    match message {
        Message::ReportPowerStatus { status } => Some(map_power(*status)),
        Message::Standby => Some(PowerState::Standby),
        Message::ActiveSource { .. } | Message::ImageViewOn | Message::TextViewOn => {
            Some(PowerState::Active)
        }
        _ => None,
    }
}

/// Map a CEC `Report Power Status` operand onto tisvlay's [`PowerState`].
///
/// `ToOn` / `ToStandby` are CEC's in-transition states; mapping them onto
/// `WarmingUp` / `CoolingDown` is a deliberate approximation — CEC has no
/// projector-style light-source warmup, but these are the nearest
/// transient states the daemon's power machine already models.
pub(crate) fn map_power(status: PowerStatus) -> PowerState {
    match status {
        PowerStatus::On => PowerState::Active,
        PowerStatus::Standby => PowerState::Standby,
        PowerStatus::ToOn => PowerState::WarmingUp,
        PowerStatus::ToStandby => PowerState::CoolingDown,
        // `PowerStatus` is `#[non_exhaustive]`; treat anything new as unknown.
        _ => PowerState::Unknown,
    }
}

/// The HDMI ports a CEC backend offers as switch targets. CEC has no
/// portable input-enumeration query, so this is a fixed best-effort set
/// (most TVs expose `HDMI 1`–`HDMI 4`); the IDs map to physical addresses
/// in [`parse_hdmi_input`].
fn default_inputs() -> Vec<InputInfo> {
    (1..=4)
        .map(|n| InputInfo {
            id: format!("HDMI_{n}"),
            label: format!("HDMI {n}"),
            connected: true,
        })
        .collect()
}

/// Map an `HDMI_N` input id to the TV-relative physical address of that
/// port. A TV — the bus root, `0.0.0.0` — addresses its HDMI port N as
/// `N.0.0.0` = `N << 12`; valid ports are 1–15.
fn parse_hdmi_input(input_id: &str) -> Option<PhysicalAddress> {
    let n: u16 = input_id.strip_prefix("HDMI_")?.parse().ok()?;
    (1..=15)
        .contains(&n)
        .then(|| PhysicalAddress::from(n << 12))
}

#[async_trait]
impl TvConnection for CecConnection {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn get_state(&mut self) -> Result<TvState> {
        let status = self.link.query_power().await?;
        let mut state = TvState::new();
        state.update_power(map_power(status));
        state.inputs = default_inputs();
        // `current_input` stays `None`: CEC has no portable "which input
        // is showing" query, so reporting unknown beats guessing.
        Ok(state)
    }

    async fn subscribe_state(&mut self) -> Result<mpsc::UnboundedReceiver<TvState>> {
        // Idempotency: abort any prior monitor before installing a new one,
        // so a re-subscribe never orphans a follower fd + its worker thread.
        if let Some(prev) = self.monitor_task.take() {
            prev.abort();
        }
        // Open a second, follower-mode handle on the same device (see
        // `CecMonitor`): its blocking receive loop lives on a distinct fd,
        // so it can't starve the single-threaded worker the tx handle
        // transmits on.
        let monitor = self.opener.open_monitor(&self.device).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            // Running snapshot re-pushed on every power-relevant change. We
            // only subscribe on a live connection, so seed it on with the
            // best-effort input set — the daemon merges partial fields, but
            // pushing a full snapshot keeps the contract identical to a
            // push backend like LG.
            let mut state = TvState::new();
            state.update_power(PowerState::Active);
            state.inputs = default_inputs();
            let mut consecutive_errors = 0u32;
            loop {
                match monitor.recv().await {
                    Ok(Some((initiator, message))) => {
                        consecutive_errors = 0;
                        // Only the TV's own broadcasts define its power —
                        // ignore the adapter's looped-back wake / Active
                        // Source frames and unrelated sources on the bus.
                        if initiator == LogicalAddress::Tv
                            && let Some(power) = power_from_message(&message)
                        {
                            state.update_power(power);
                            if tx.send(state.clone()).is_err() {
                                break; // daemon dropped the subscription
                            }
                        }
                    }
                    // Idle beat: stop once the consumer is gone.
                    Ok(None) => {
                        consecutive_errors = 0;
                        if tx.is_closed() {
                            break;
                        }
                    }
                    Err(e) => {
                        // A real disconnect ends the stream promptly (drives
                        // the daemon's subscription-end ConnectionChanged); a
                        // transient rx error (garbled frame, momentary EBUSY)
                        // backs off and retries rather than killing the
                        // subscription for the life of the connection.
                        if matches!(e, Error::ConnectionClosed) {
                            break;
                        }
                        consecutive_errors += 1;
                        if consecutive_errors >= MONITOR_MAX_ERRORS {
                            break;
                        }
                        tokio::time::sleep(MONITOR_ERROR_BACKOFF).await;
                    }
                }
            }
        });
        self.monitor_task = Some(task);
        Ok(rx)
    }

    async fn close(&mut self) -> Result<()> {
        // Mark the handle closed; the worker thread is torn down when the
        // boxed link drops with this connection. `close` itself stays
        // non-blocking so the daemon's per-TV lock isn't held on a join.
        self.connected = false;
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::POWER | Capabilities::INPUT
    }

    async fn power_off(&mut self) -> Result<()> {
        self.link.power_off().await?;
        // The TV is heading to standby; drop the reachable-for-control
        // flag so the daemon stops treating it as live.
        self.connected = false;
        Ok(())
    }

    async fn get_power_state(&mut self) -> Result<PowerState> {
        Ok(map_power(self.link.query_power().await?))
    }

    async fn switch_input(&mut self, input_id: &str) -> Result<()> {
        let address = parse_hdmi_input(input_id).ok_or_else(|| {
            Error::backend(
                crate::VENDOR_ID,
                format!("unknown CEC input {input_id:?} (expected HDMI_1..HDMI_15)"),
            )
        })?;
        self.link.set_stream_path(address).await
    }

    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        // Best-effort fixed set — CEC can't enumerate the TV's ports.
        Ok(default_inputs())
    }

    async fn get_current_input(&mut self) -> Result<Option<String>> {
        // Not portably readable over CEC; report unknown.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{FakeCecLink, FakeCecLinkOpener};

    fn connection(power: PowerStatus) -> CecConnection {
        CecConnection::new(
            Box::new(FakeCecLink::new(power)),
            Arc::new(FakeCecLinkOpener::new(power)),
            "/dev/cec0".to_string(),
        )
    }

    #[test]
    fn power_status_maps_to_power_state() {
        assert_eq!(map_power(PowerStatus::On), PowerState::Active);
        assert_eq!(map_power(PowerStatus::Standby), PowerState::Standby);
        assert_eq!(map_power(PowerStatus::ToOn), PowerState::WarmingUp);
        assert_eq!(map_power(PowerStatus::ToStandby), PowerState::CoolingDown);
    }

    #[tokio::test]
    async fn get_state_reports_live_power_and_inputs() {
        let mut conn = connection(PowerStatus::On);
        let state = conn.get_state().await.expect("state");
        assert_eq!(state.power_state, PowerState::Active);
        assert!(state.is_on);
        // CEC reports no volume/backlight — those Options stay None so
        // capability-gated clients hide the controls.
        assert_eq!(state.volume, None);
        assert_eq!(state.backlight, None);
        // Inputs are the best-effort HDMI set; current input is unknown.
        assert_eq!(state.inputs.len(), 4);
        assert_eq!(state.current_input, None);
    }

    #[tokio::test]
    async fn subscribe_state_pushes_power_changes_from_the_bus() {
        // The monitor replays an unsolicited <Standby> (what a TV
        // broadcasts when powered off at the set); the subscription must
        // push a snapshot flipped to Standby, with no poll involved.
        let opener = Arc::new(FakeCecLinkOpener::with_monitor_script(vec![
            Message::Standby,
        ]));
        let mut conn = CecConnection::new(
            Box::new(FakeCecLink::new(PowerStatus::On)),
            opener,
            "/dev/cec0".to_string(),
        );
        let mut rx = conn.subscribe_state().await.expect("subscribe");
        let state = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("state pushed within 1s")
            .expect("subscription channel open");
        assert_eq!(state.power_state, PowerState::Standby);
        // Full snapshot — the best-effort input set rides along.
        assert_eq!(state.inputs.len(), 4);
    }

    #[tokio::test]
    async fn dropping_connection_ends_the_subscription() {
        // The monitor task is a detached spawn; on connection drop it must
        // be aborted so the state channel closes. The daemon's
        // subscription-end path keys on that closure to emit
        // `ConnectionChanged(false)` — without it a powered-off CEC TV
        // would read `Connected` forever.
        let opener = Arc::new(FakeCecLinkOpener::with_monitor_script(vec![]));
        let mut conn = CecConnection::new(
            Box::new(FakeCecLink::new(PowerStatus::On)),
            opener,
            "/dev/cec0".to_string(),
        );
        let mut rx = conn.subscribe_state().await.expect("subscribe");
        drop(conn);
        assert!(
            rx.recv().await.is_none(),
            "state channel closes once the connection drops",
        );
    }

    #[tokio::test]
    async fn subscribe_state_ignores_non_tv_initiators() {
        // A <Standby> that did not originate from the TV — the adapter's own
        // looped-back frame, or an unrelated bus device — must not flip the
        // reported power.
        let opener = Arc::new(FakeCecLinkOpener::with_monitor_script_from(vec![(
            LogicalAddress::Tuner1,
            Message::Standby,
        )]));
        let mut conn = CecConnection::new(
            Box::new(FakeCecLink::new(PowerStatus::On)),
            opener,
            "/dev/cec0".to_string(),
        );
        let mut rx = conn.subscribe_state().await.expect("subscribe");
        let got = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
        assert!(
            got.is_err(),
            "a non-TV initiator must be filtered (no push)"
        );
    }

    #[tokio::test]
    async fn power_off_sends_standby_and_marks_disconnected() {
        let mut conn = connection(PowerStatus::On);
        conn.power_off().await.expect("power off");
        assert!(!conn.is_connected());
        // The fake flips to standby; a fresh state read reflects it.
        assert_eq!(
            conn.get_power_state().await.expect("power state"),
            PowerState::Standby
        );
    }

    #[tokio::test]
    async fn close_is_non_blocking_and_marks_disconnected() {
        let mut conn = connection(PowerStatus::On);
        conn.close().await.expect("close");
        assert!(!conn.is_connected());
    }

    #[test]
    fn capabilities_are_power_and_input() {
        let conn = connection(PowerStatus::On);
        assert_eq!(
            conn.capabilities(),
            Capabilities::POWER | Capabilities::INPUT
        );
    }

    #[test]
    fn parse_hdmi_input_maps_ports_to_physical_addresses() {
        assert_eq!(
            parse_hdmi_input("HDMI_1"),
            Some(PhysicalAddress::from(0x1000))
        );
        assert_eq!(
            parse_hdmi_input("HDMI_2"),
            Some(PhysicalAddress::from(0x2000))
        );
        assert_eq!(
            parse_hdmi_input("HDMI_4"),
            Some(PhysicalAddress::from(0x4000))
        );
        // Out of range / malformed → no address.
        assert_eq!(parse_hdmi_input("HDMI_0"), None);
        assert_eq!(parse_hdmi_input("HDMI_16"), None);
        assert_eq!(parse_hdmi_input("HDMI_x"), None);
        assert_eq!(parse_hdmi_input("VGA_1"), None);
    }

    #[tokio::test]
    async fn switch_input_accepts_hdmi_port() {
        let mut conn = connection(PowerStatus::On);
        assert!(conn.switch_input("HDMI_2").await.is_ok());
    }

    #[tokio::test]
    async fn switch_input_rejects_unknown_input() {
        let mut conn = connection(PowerStatus::On);
        assert!(conn.switch_input("not-a-port").await.is_err());
    }

    #[tokio::test]
    async fn list_inputs_returns_the_hdmi_ports() {
        let mut conn = connection(PowerStatus::On);
        let inputs = conn.list_inputs().await.expect("inputs");
        let ids: Vec<_> = inputs.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["HDMI_1", "HDMI_2", "HDMI_3", "HDMI_4"]);
    }
}
