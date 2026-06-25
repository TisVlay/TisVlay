//! `CecConnection` — a live CEC control handle for one TV.
//!
//! v1 surface is power: report state, and turn off (power-on is the
//! connector's wake path, since the bus reaches the TV even in standby).
//! Active-source input switching lands in the next slice; everything else
//! stays at the trait's `NotSupported` default.

use async_trait::async_trait;
use linux_cec::operand::PowerStatus;
use tisvlay_core::{Capabilities, PowerState, Result, TvConnection, TvState};

use crate::link::CecLink;

/// A live CEC control handle for one TV.
#[derive(Debug)]
pub struct CecConnection {
    link: Box<dyn CecLink>,
    connected: bool,
}

impl CecConnection {
    pub(crate) fn new(link: Box<dyn CecLink>) -> Self {
        Self {
            link,
            connected: true,
        }
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
        Ok(state)
    }

    async fn close(&mut self) -> Result<()> {
        // Mark the handle closed; the worker thread is torn down when the
        // boxed link drops with this connection. `close` itself stays
        // non-blocking so the daemon's per-TV lock isn't held on a join.
        self.connected = false;
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::POWER
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::FakeCecLink;

    fn connection(power: PowerStatus) -> CecConnection {
        CecConnection::new(Box::new(FakeCecLink::new(power)))
    }

    #[test]
    fn power_status_maps_to_power_state() {
        assert_eq!(map_power(PowerStatus::On), PowerState::Active);
        assert_eq!(map_power(PowerStatus::Standby), PowerState::Standby);
        assert_eq!(map_power(PowerStatus::ToOn), PowerState::WarmingUp);
        assert_eq!(map_power(PowerStatus::ToStandby), PowerState::CoolingDown);
    }

    #[tokio::test]
    async fn get_state_reports_live_power() {
        let mut conn = connection(PowerStatus::On);
        let state = conn.get_state().await.expect("state");
        assert_eq!(state.power_state, PowerState::Active);
        assert!(state.is_on);
        // CEC v1 reports no volume/backlight — those Options stay None so
        // capability-gated clients hide the controls.
        assert_eq!(state.volume, None);
        assert_eq!(state.backlight, None);
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
    fn capabilities_are_power_only_in_v1() {
        let conn = connection(PowerStatus::On);
        assert_eq!(conn.capabilities(), Capabilities::POWER);
    }
}
