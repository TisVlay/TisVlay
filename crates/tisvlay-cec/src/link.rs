//! The CEC transport seam.
//!
//! `linux-cec` ships no fake server, so this trait is tisvlay-cec's
//! testability boundary: production wraps [`linux_cec::device::AsyncDevice`],
//! and tests use a scripted `FakeCecLink`. It deliberately sits one layer
//! lower than LG's `FakeSsapServer` (which fakes the *peer* and drives the
//! real transport) — here we fake the *library wrapper*, so unit tests
//! never touch a real `/dev/cecN` ioctl. Daemon-level coverage of the
//! always-reachable state machine rides the mock backend, not this seam.

use async_trait::async_trait;
use linux_cec::device::{AsyncDevice, MessageData};
use linux_cec::message::{Message, Opcode};
use linux_cec::operand::PowerStatus;
use linux_cec::{LogicalAddress, LogicalAddressType, Timeout};
use tisvlay_core::{Capabilities, Error, Result};

use crate::VENDOR_ID;

/// Opens a [`CecLink`] for a given device path.
///
/// The factory seam: production opens a real `/dev/cecN`; tests inject a
/// fake that yields a scripted link with no hardware.
#[async_trait]
pub(crate) trait CecLinkOpener: Send + Sync + std::fmt::Debug {
    async fn open(&self, device: &str) -> Result<Box<dyn CecLink>>;
}

/// A live handle to one CEC adapter, scoped to exactly the operations the
/// connector and connection need. Every method takes `&self` (the
/// underlying `AsyncDevice` relays to its worker thread), so a connection
/// can drive it through a shared reference.
#[async_trait]
pub(crate) trait CecLink: Send + Sync + std::fmt::Debug {
    /// Query the TV's power status (`Give Device Power Status` →
    /// `Report Power Status`).
    async fn query_power(&self) -> Result<PowerStatus>;
    /// Wake the TV (`Image View On`) and announce ourselves as the active
    /// source so it switches to our input.
    async fn power_on(&self) -> Result<()>;
    /// Put the TV into standby (`Standby`).
    async fn power_off(&self) -> Result<()>;
}

/// Production [`CecLinkOpener`] backed by `linux_cec::AsyncDevice`.
#[derive(Debug)]
pub(crate) struct LinuxCecLinkOpener;

#[async_trait]
impl CecLinkOpener for LinuxCecLinkOpener {
    async fn open(&self, device: &str) -> Result<Box<dyn CecLink>> {
        Ok(Box::new(LinuxCecLink::open(device).await?))
    }
}

/// The CEC ceiling for a single message round-trip is one second; use it
/// for the power-status query.
const POWER_QUERY_TIMEOUT: Timeout = Timeout::MAX;

/// Production [`CecLink`] wrapping a `linux_cec::AsyncDevice`.
///
/// `AsyncDevice` runs a dedicated worker thread that it joins on drop.
/// Since the daemon drops a connection (and thus this link) while holding
/// the per-TV mutex, that join is bounded by the in-flight ioctl — CEC
/// message timeouts cap at one second.
#[derive(Debug)]
struct LinuxCecLink {
    device: AsyncDevice,
}

impl LinuxCecLink {
    async fn open(device: &str) -> Result<Self> {
        let dev = AsyncDevice::open(device).await.map_err(map_cec_err)?;
        // Claim a Playback logical address so the adapter is allowed to
        // transmit on the bus.
        dev.set_logical_address(LogicalAddressType::Playback)
            .await
            .map_err(map_cec_err)?;
        Ok(Self { device: dev })
    }
}

#[async_trait]
impl CecLink for LinuxCecLink {
    async fn query_power(&self) -> Result<PowerStatus> {
        let envelope = self
            .device
            .tx_rx_message(
                &Message::GiveDevicePowerStatus {},
                LogicalAddress::Tv,
                Opcode::ReportPowerStatus,
                POWER_QUERY_TIMEOUT,
            )
            .await
            .map_err(map_cec_err)?;
        match envelope.message {
            MessageData::Valid(Message::ReportPowerStatus { status }) => Ok(status),
            other => Err(Error::backend(
                VENDOR_ID,
                format!("expected Report Power Status, got {other:?}"),
            )),
        }
    }

    async fn power_on(&self) -> Result<()> {
        // `wake(set_active = true, text_view = false)` transmits
        // Image View On and then broadcasts Active Source.
        self.device.wake(true, false).await.map_err(map_cec_err)
    }

    async fn power_off(&self) -> Result<()> {
        self.device
            .standby(LogicalAddress::Tv)
            .await
            .map_err(map_cec_err)
    }
}

/// Map a `linux_cec::Error` onto the vendor-agnostic [`Error`].
fn map_cec_err(e: linux_cec::Error) -> Error {
    use linux_cec::Error as L;
    match e {
        L::Unsupported => Error::not_supported("CEC operation", Capabilities::empty()),
        L::Timeout => Error::RequestTimeout,
        L::Disconnected => Error::ConnectionClosed,
        other => Error::backend_with_source(VENDOR_ID, "CEC error", other),
    }
}

#[cfg(test)]
pub(crate) use fake::{FakeCecLink, FakeCecLinkOpener};

#[cfg(test)]
mod fake {
    use super::*;
    use parking_lot::Mutex;

    /// A scripted [`CecLink`] for tests: holds a mutable power status that
    /// `power_on` / `power_off` flip, so connector and connection logic
    /// run without a real `/dev/cecN`.
    #[derive(Debug)]
    pub(crate) struct FakeCecLink {
        power: Mutex<PowerStatus>,
    }

    impl FakeCecLink {
        pub(crate) fn new(power: PowerStatus) -> Self {
            Self {
                power: Mutex::new(power),
            }
        }
    }

    #[async_trait]
    impl CecLink for FakeCecLink {
        async fn query_power(&self) -> Result<PowerStatus> {
            Ok(*self.power.lock())
        }

        async fn power_on(&self) -> Result<()> {
            *self.power.lock() = PowerStatus::On;
            Ok(())
        }

        async fn power_off(&self) -> Result<()> {
            *self.power.lock() = PowerStatus::Standby;
            Ok(())
        }
    }

    /// Opener that yields a [`FakeCecLink`] seeded with a fixed initial
    /// power status, regardless of the requested device path.
    #[derive(Debug)]
    pub(crate) struct FakeCecLinkOpener {
        initial: PowerStatus,
    }

    impl FakeCecLinkOpener {
        pub(crate) fn new(initial: PowerStatus) -> Self {
            Self { initial }
        }
    }

    #[async_trait]
    impl CecLinkOpener for FakeCecLinkOpener {
        async fn open(&self, _device: &str) -> Result<Box<dyn CecLink>> {
            Ok(Box::new(FakeCecLink::new(self.initial)))
        }
    }
}
