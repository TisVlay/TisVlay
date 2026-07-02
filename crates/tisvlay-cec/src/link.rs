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
use linux_cec::device::{AsyncDevice, Capabilities as CecCapabilities, MessageData};
use linux_cec::message::{Message, Opcode};
use linux_cec::operand::PowerStatus;
use linux_cec::{
    FollowerMode, InitiatorMode, LogicalAddress, LogicalAddressType, PhysicalAddress, Timeout,
};
use tisvlay_core::{Capabilities, Error, Result};

use crate::VENDOR_ID;

/// Opens a [`CecLink`] for a given device path.
///
/// The factory seam: production opens a real `/dev/cecN`; tests inject a
/// fake that yields a scripted link with no hardware.
#[async_trait]
pub(crate) trait CecLinkOpener: Send + Sync + std::fmt::Debug {
    async fn open(
        &self,
        device: &str,
        physical_address: PhysicalAddress,
    ) -> Result<Box<dyn CecLink>>;

    /// Open a *second*, follower-mode handle on the same device for
    /// receiving unsolicited traffic. It shares the adapter's claimed
    /// logical address with the tx [`CecLink`] but is a distinct fd — so
    /// its blocking receive loop can't starve the single-threaded
    /// `AsyncDevice` worker the connection transmits on.
    async fn open_monitor(&self, device: &str) -> Result<Box<dyn CecMonitor>>;
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
    /// Ask the TV to switch to the input at `address` (`Set Stream Path`,
    /// a broadcast that tells whichever device owns that physical address
    /// to become the active source).
    async fn set_stream_path(&self, address: PhysicalAddress) -> Result<()>;
}

/// A follower-mode handle for receiving unsolicited CEC traffic — the
/// receive half of push-based state (power/input changes the TV makes on
/// its own, e.g. via its remote). Kept separate from [`CecLink`] because
/// the two are distinct fds on the same adapter.
#[async_trait]
pub(crate) trait CecMonitor: Send + Sync + std::fmt::Debug {
    /// Await the next inbound message. `Ok(None)` on a receive timeout
    /// (the caller loops, re-checking whether it should keep listening);
    /// `Err` once the adapter goes away.
    async fn recv(&self) -> Result<Option<Message>>;
}

/// Production [`CecLinkOpener`] backed by `linux_cec::AsyncDevice`.
#[derive(Debug)]
pub(crate) struct LinuxCecLinkOpener;

#[async_trait]
impl CecLinkOpener for LinuxCecLinkOpener {
    async fn open(
        &self,
        device: &str,
        physical_address: PhysicalAddress,
    ) -> Result<Box<dyn CecLink>> {
        Ok(Box::new(
            LinuxCecLink::open(device, physical_address).await?,
        ))
    }

    async fn open_monitor(&self, device: &str) -> Result<Box<dyn CecMonitor>> {
        Ok(Box::new(LinuxCecMonitor::open(device).await?))
    }
}

/// Poll cadence for the monitor receive loop. `Timeout::MAX` is the CEC
/// spec's 1 s round-trip ceiling; a receive that hits it returns
/// `Ok(None)`, giving the subscription task a regular beat to notice its
/// consumer went away and shut down.
const MONITOR_RX_POLL: Timeout = Timeout::MAX;

/// Production [`CecMonitor`]: a follower-mode `AsyncDevice` on the same
/// node as the tx link. It claims no logical address of its own — the tx
/// link already claimed one for the adapter — and just listens.
#[derive(Debug)]
struct LinuxCecMonitor {
    device: AsyncDevice,
}

impl LinuxCecMonitor {
    async fn open(device: &str) -> Result<Self> {
        let dev = AsyncDevice::open(device).await.map_err(map_cec_err)?;
        dev.set_follower_mode(FollowerMode::Enabled)
            .await
            .map_err(map_cec_err)?;
        Ok(Self { device: dev })
    }
}

#[async_trait]
impl CecMonitor for LinuxCecMonitor {
    async fn recv(&self) -> Result<Option<Message>> {
        match self.device.rx_message(MONITOR_RX_POLL).await {
            Ok(envelope) => match envelope.message {
                MessageData::Valid(message) => Ok(Some(message)),
                // Malformed / aborted frame — ignore, keep listening.
                _ => Ok(None),
            },
            // A receive timeout is the normal idle case, not an error.
            Err(linux_cec::Error::Timeout) => Ok(None),
            Err(e) => Err(map_cec_err(e)),
        }
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
    async fn open(device: &str, physical_address: PhysicalAddress) -> Result<Self> {
        let dev = AsyncDevice::open(device).await.map_err(map_cec_err)?;
        // Enable transmit before configuring any addresses.
        dev.set_initiator_mode(InitiatorMode::Enabled)
            .await
            .map_err(map_cec_err)?;
        // A USB adapter advertises CEC_CAP_PHYS_ADDR: the kernel can't
        // derive its physical address (there is no DRM connector to read an
        // EDID from), so we set one — otherwise the logical-address claim
        // below never takes effect on the bus. GPU DisplayPort-CEC lacks
        // the cap (the kernel already assigned an address from the
        // connector), so we leave that path untouched.
        if dev
            .get_capabilities()
            .await
            .map_err(map_cec_err)?
            .contains(CecCapabilities::PHYS_ADDR)
        {
            dev.set_physical_address(physical_address)
                .await
                .map_err(map_cec_err)?;
        }
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

    async fn set_stream_path(&self, address: PhysicalAddress) -> Result<()> {
        // Set Stream Path is a broadcast message.
        self.device
            .tx_message(
                &Message::SetStreamPath { address },
                LogicalAddress::UnregisteredOrBroadcast,
            )
            .await
            .map(|_seq| ())
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
    use std::collections::VecDeque;

    /// A scripted [`CecLink`] for tests: holds a mutable power status that
    /// `power_on` / `power_off` flip, so connector and connection logic
    /// run without a real `/dev/cecN`. `ignore_wakes` models a TV that
    /// drops the first N `power_on` sends before reporting on, so tests
    /// can exercise the wake re-send loop.
    #[derive(Debug)]
    pub(crate) struct FakeCecLink {
        power: Mutex<PowerStatus>,
        ignore_wakes: Mutex<usize>,
    }

    impl FakeCecLink {
        pub(crate) fn new(power: PowerStatus) -> Self {
            Self {
                power: Mutex::new(power),
                ignore_wakes: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl CecLink for FakeCecLink {
        async fn query_power(&self) -> Result<PowerStatus> {
            Ok(*self.power.lock())
        }

        async fn power_on(&self) -> Result<()> {
            // Drop the first `ignore_wakes` sends (they "don't land"),
            // then honor the next one — modeling a flaky/just-standby TV.
            let mut remaining = self.ignore_wakes.lock();
            if *remaining == 0 {
                *self.power.lock() = PowerStatus::On;
            } else {
                *remaining -= 1;
            }
            Ok(())
        }

        async fn power_off(&self) -> Result<()> {
            *self.power.lock() = PowerStatus::Standby;
            Ok(())
        }

        async fn set_stream_path(&self, _address: PhysicalAddress) -> Result<()> {
            Ok(())
        }
    }

    /// Opener that yields a [`FakeCecLink`] seeded with a fixed initial
    /// power status, regardless of the requested device path.
    #[derive(Debug)]
    pub(crate) struct FakeCecLinkOpener {
        initial: PowerStatus,
        ignore_wakes: usize,
        /// Scripted inbound messages the first `open_monitor` hands to its
        /// [`FakeCecMonitor`], for `subscribe_state` tests. `take`-n once.
        monitor_script: Mutex<Option<VecDeque<Message>>>,
    }

    impl FakeCecLinkOpener {
        pub(crate) fn new(initial: PowerStatus) -> Self {
            Self {
                initial,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
            }
        }

        /// Opener yielding a standby TV that only wakes after `ignored`
        /// dropped `power_on` sends — exercises the wake re-send loop.
        pub(crate) fn new_flaky_wake(ignored: usize) -> Self {
            Self {
                initial: PowerStatus::Standby,
                ignore_wakes: ignored,
                monitor_script: Mutex::new(None),
            }
        }

        /// Opener whose monitor handle replays `script` (one message per
        /// `recv`), then idles — drives `subscribe_state` tests.
        pub(crate) fn with_monitor_script(script: Vec<Message>) -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(Some(script.into())),
            }
        }
    }

    #[async_trait]
    impl CecLinkOpener for FakeCecLinkOpener {
        async fn open(
            &self,
            _device: &str,
            _physical_address: PhysicalAddress,
        ) -> Result<Box<dyn CecLink>> {
            Ok(Box::new(FakeCecLink {
                power: Mutex::new(self.initial),
                ignore_wakes: Mutex::new(self.ignore_wakes),
            }))
        }

        async fn open_monitor(&self, _device: &str) -> Result<Box<dyn CecMonitor>> {
            let script = self.monitor_script.lock().take().unwrap_or_default();
            Ok(Box::new(FakeCecMonitor {
                script: Mutex::new(script),
            }))
        }
    }

    /// Replays a fixed script of inbound messages, then idles (yielding so
    /// the subscription task can notice its consumer went away).
    #[derive(Debug)]
    pub(crate) struct FakeCecMonitor {
        script: Mutex<VecDeque<Message>>,
    }

    #[async_trait]
    impl CecMonitor for FakeCecMonitor {
        async fn recv(&self) -> Result<Option<Message>> {
            let next = self.script.lock().pop_front();
            match next {
                Some(message) => Ok(Some(message)),
                None => {
                    tokio::task::yield_now().await;
                    Ok(None)
                }
            }
        }
    }
}
