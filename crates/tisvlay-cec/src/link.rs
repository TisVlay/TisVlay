//! The CEC transport seam.
//!
//! `linux-cec` ships no fake server, so this trait is tisvlay-cec's
//! testability boundary: production wraps [`linux_cec::device::AsyncDevice`],
//! and tests use a scripted `FakeCecLink`. It deliberately sits one layer
//! lower than LG's `FakeSsapServer` (which fakes the *peer* and drives the
//! real transport) — here we fake the *library wrapper*, so unit tests
//! never touch a real `/dev/cecN` ioctl. Daemon-level coverage of the
//! always-reachable state machine rides the mock backend, not this seam.

use std::time::Duration;

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

    /// The TV's self-reported name (`Give OSD Name` → `Set OSD Name`), e.g.
    /// `OLED42C3PU` — becomes the `HardwareId` model. Resolved to identity
    /// via [`crate::identity`].
    async fn query_osd_name(&self) -> Result<String>;
    /// The TV's vendor OUI (`Give Device Vendor ID` → `Device Vendor ID`);
    /// `None` when the TV reports no vendor. Resolved to the manufacturer +
    /// tile brand via [`crate::identity`].
    async fn query_vendor_id(&self) -> Result<Option<linux_cec::VendorId>>;
}

/// A follower-mode handle for receiving unsolicited CEC traffic — the
/// receive half of push-based state (power/input changes the TV makes on
/// its own, e.g. via its remote). Kept separate from [`CecLink`] because
/// the two are distinct fds on the same adapter.
#[async_trait]
pub(crate) trait CecMonitor: Send + Sync + std::fmt::Debug {
    /// Await the next inbound message, paired with its initiator (so the
    /// caller can ignore the adapter's own looped-back frames). `Ok(None)`
    /// on a receive timeout (the caller loops, re-checking whether it
    /// should keep listening); `Err` on a transient or fatal rx error.
    async fn recv(&self) -> Result<Option<(LogicalAddress, Message)>>;
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

/// Idle receive cadence for the monitor loop, in milliseconds. Kept well
/// under the 1 s CEC ceiling for two reasons: it is the beat on which the
/// loop notices its consumer went away, and it bounds the worker-thread
/// `join()` in `AsyncDevice::drop` (which runs on the executor when the
/// aborted monitor task is dropped) to roughly this instead of a second.
const MONITOR_RX_POLL_MS: u64 = 250;

/// Production [`CecMonitor`]: a follower-mode `AsyncDevice` on the same
/// node as the tx link. It claims no logical address of its own — the tx
/// link already claimed one for the adapter — and just listens.
#[derive(Debug)]
struct LinuxCecMonitor {
    device: AsyncDevice,
}

impl LinuxCecMonitor {
    async fn open(device: &str) -> Result<Self> {
        let dev = AsyncDevice::open(device)
            .await
            .map_err(|e| map_open_err(device, e))?;
        dev.set_follower_mode(FollowerMode::Enabled)
            .await
            .map_err(map_cec_err)?;
        Ok(Self { device: dev })
    }
}

#[async_trait]
impl CecMonitor for LinuxCecMonitor {
    async fn recv(&self) -> Result<Option<(LogicalAddress, Message)>> {
        let timeout =
            Timeout::try_from(&Duration::from_millis(MONITOR_RX_POLL_MS)).unwrap_or(Timeout::MAX);
        match self.device.rx_message(timeout).await {
            Ok(envelope) => match envelope.message {
                MessageData::Valid(message) => Ok(Some((envelope.initiator, message))),
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

/// Bound for the best-effort identity queries (OSD name, vendor). Unlike
/// power, identity is optional — a TV that never answers `Give OSD Name`
/// must not stall connect or discovery, so this is a short, finite wait
/// (the CEC spec's own response window is ~1 s) rather than `Timeout::MAX`.
fn identity_query_timeout() -> Timeout {
    Timeout::from_ms(2000)
}

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
        let dev = AsyncDevice::open(device)
            .await
            .map_err(|e| map_open_err(device, e))?;
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

    async fn query_osd_name(&self) -> Result<String> {
        // Query the *TV's* name over the bus (`Give OSD Name` →
        // `Set OSD Name`), NOT `AsyncDevice::get_osd_name()`, which returns
        // our own adapter's locally-configured, fixed 14-byte NUL-padded
        // buffer. The message operand carries only the actual bytes (no
        // padding); `identity::query` still sanitizes for D-Bus safety.
        let envelope = self
            .device
            .tx_rx_message(
                &Message::GiveOsdName {},
                LogicalAddress::Tv,
                Opcode::SetOsdName,
                identity_query_timeout(),
            )
            .await
            .map_err(map_cec_err)?;
        match envelope.message {
            MessageData::Valid(Message::SetOsdName { name }) => {
                Ok(String::from_utf8_lossy(name.as_bytes()).into_owned())
            }
            other => Err(Error::backend(
                VENDOR_ID,
                format!("expected Set OSD Name, got {other:?}"),
            )),
        }
    }

    async fn query_vendor_id(&self) -> Result<Option<linux_cec::VendorId>> {
        // The TV's vendor over the bus (`Give Device Vendor ID` →
        // `Device Vendor ID`), not our adapter's local one.
        let envelope = self
            .device
            .tx_rx_message(
                &Message::GiveDeviceVendorId {},
                LogicalAddress::Tv,
                Opcode::DeviceVendorId,
                identity_query_timeout(),
            )
            .await
            .map_err(map_cec_err)?;
        match envelope.message {
            MessageData::Valid(Message::DeviceVendorId { vendor_id }) => Ok(Some(vendor_id)),
            other => Err(Error::backend(
                VENDOR_ID,
                format!("expected Device Vendor ID, got {other:?}"),
            )),
        }
    }
}

/// Map a `linux_cec::Error` onto the vendor-agnostic [`Error`].
fn map_cec_err(e: linux_cec::Error) -> Error {
    use linux_cec::Error as L;
    match e {
        L::Unsupported => Error::not_supported("CEC operation", Capabilities::empty()),
        L::Timeout => Error::RequestTimeout,
        L::Disconnected => Error::ConnectionClosed,
        // Render the underlying error into the message (not just "CEC
        // error") so a stray errno reaches the log. Not also passed as
        // `#[source]` — that would double-render the errno under chain /
        // alternate formatting (`{:#}`).
        other => Error::backend(VENDOR_ID, format!("CEC error: {other}")),
    }
}

/// Map a `linux_cec::Error` from *opening* a device node, attaching the
/// path. A permission failure (`EACCES` / `EPERM` — the daemon's user
/// isn't in the group owning the node, or the udev rule isn't installed)
/// becomes the typed, transport-agnostic [`Error::PermissionDenied`] so
/// clients can surface an actionable message; anything else falls back to
/// [`map_cec_err`].
fn map_open_err(device: &str, e: linux_cec::Error) -> Error {
    use linux_cec::Error as L;
    // Compare the raw errno against the ABI-stable `libc` constants rather
    // than naming `nix::errno::Errno` variants: `linux-cec` surfaces the
    // errno as its own transitively-selected `nix::Errno`, and matching the
    // integer value keeps this classification from pinning us to whatever
    // `nix` major that happens to be.
    if let L::SystemError(errno) = &e {
        let raw = *errno as i32;
        if raw == libc::EACCES || raw == libc::EPERM {
            return Error::PermissionDenied {
                path: device.to_string(),
            };
        }
    }
    map_cec_err(e)
}

#[cfg(test)]
mod map_tests {
    use super::*;

    // Exercise the REAL classification path — an `io::Error` with an errno,
    // through `linux_cec`'s `From<io::Error>` into `SystemError(errno)`,
    // into `map_open_err` — rather than the `FakeCecLinkOpener` shortcut
    // (which returns `Error::PermissionDenied` directly). Guards against a
    // `linux-cec` change that remaps open failures away from `SystemError`.
    #[test]
    fn map_open_err_classifies_eacces_eperm_as_permission_denied() {
        for raw in [libc::EACCES, libc::EPERM] {
            let e = linux_cec::Error::from(std::io::Error::from_raw_os_error(raw));
            let mapped = map_open_err("/dev/cec0", e);
            assert!(
                matches!(mapped, Error::PermissionDenied { ref path } if path == "/dev/cec0"),
                "errno {raw} should map to PermissionDenied, got {mapped:?}",
            );
        }
    }

    #[test]
    fn map_open_err_passes_other_errnos_through() {
        let e = linux_cec::Error::from(std::io::Error::from_raw_os_error(libc::EBUSY));
        assert!(
            !matches!(map_open_err("/dev/cec0", e), Error::PermissionDenied { .. }),
            "a non-permission errno must not classify as PermissionDenied",
        );
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
        /// Scripted identity for `query_osd_name` / `query_vendor_id`.
        /// Defaults are empty/`None` (no identity) so existing tests, which
        /// don't assert identity, see an all-absent result.
        osd_name: String,
        vendor_id: Option<linux_cec::VendorId>,
    }

    impl FakeCecLink {
        pub(crate) fn new(power: PowerStatus) -> Self {
            Self {
                power: Mutex::new(power),
                ignore_wakes: Mutex::new(0),
                osd_name: String::new(),
                vendor_id: None,
            }
        }

        /// Seed the scripted TV identity the connection/discovery queries.
        pub(crate) fn with_identity(
            mut self,
            osd_name: &str,
            vendor_id: Option<linux_cec::VendorId>,
        ) -> Self {
            self.osd_name = osd_name.to_string();
            self.vendor_id = vendor_id;
            self
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

        async fn query_osd_name(&self) -> Result<String> {
            Ok(self.osd_name.clone())
        }

        async fn query_vendor_id(&self) -> Result<Option<linux_cec::VendorId>> {
            Ok(self.vendor_id)
        }
    }

    /// Opener that yields a [`FakeCecLink`] seeded with a fixed initial
    /// power status, regardless of the requested device path.
    #[derive(Debug)]
    pub(crate) struct FakeCecLinkOpener {
        initial: PowerStatus,
        ignore_wakes: usize,
        /// Scripted inbound (initiator, message) pairs the first
        /// `open_monitor` hands to its [`FakeCecMonitor`], for
        /// `subscribe_state` tests. `take`-n once.
        monitor_script: Mutex<Option<VecDeque<(LogicalAddress, Message)>>>,
        /// When set, the monitor's `recv` returns `ConnectionClosed` once
        /// its script drains — models a real adapter disconnect.
        monitor_fails: bool,
        /// Scripted `(osd_name, vendor_id)` the opened [`FakeCecLink`]
        /// reports; `None` = no identity.
        identity: Option<(String, Option<linux_cec::VendorId>)>,
        /// When set, `open` fails with [`Error::PermissionDenied`] —
        /// models the daemon's user lacking access to the node, for the
        /// discovery-badge and connect-failure paths.
        deny_permission: bool,
    }

    impl FakeCecLinkOpener {
        pub(crate) fn new(initial: PowerStatus) -> Self {
            Self {
                initial,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: None,
                deny_permission: false,
            }
        }

        /// Opener yielding a standby TV that only wakes after `ignored`
        /// dropped `power_on` sends — exercises the wake re-send loop.
        pub(crate) fn new_flaky_wake(ignored: usize) -> Self {
            Self {
                initial: PowerStatus::Standby,
                ignore_wakes: ignored,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: None,
                deny_permission: false,
            }
        }

        /// Opener whose monitor handle replays `script` (one message per
        /// `recv`) as if each came from the TV, then idles — drives
        /// `subscribe_state` tests.
        pub(crate) fn with_monitor_script(script: Vec<Message>) -> Self {
            Self::with_monitor_script_from(
                script
                    .into_iter()
                    .map(|message| (LogicalAddress::Tv, message))
                    .collect(),
            )
        }

        /// Like [`Self::with_monitor_script`] but with explicit initiators,
        /// so tests can exercise the monitor's initiator filter.
        pub(crate) fn with_monitor_script_from(script: Vec<(LogicalAddress, Message)>) -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(Some(script.into())),
                monitor_fails: false,
                identity: None,
                deny_permission: false,
            }
        }

        /// Opener whose opened links report the given TV identity — for the
        /// discovery-probe tests.
        pub(crate) fn with_identity(
            osd_name: &str,
            vendor_id: Option<linux_cec::VendorId>,
        ) -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: Some((osd_name.to_string(), vendor_id)),
                deny_permission: false,
            }
        }

        /// Opener whose `open` fails with [`Error::PermissionDenied`],
        /// modeling a node the daemon's user can enumerate but not open.
        pub(crate) fn permission_denied() -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: None,
                deny_permission: true,
            }
        }

        /// Opener whose monitor `recv` returns `ConnectionClosed` — models a
        /// real adapter disconnect, so tests can assert the monitor flips
        /// `is_connected()`.
        pub(crate) fn with_monitor_disconnect() -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: true,
                identity: None,
                deny_permission: false,
            }
        }
    }

    #[async_trait]
    impl CecLinkOpener for FakeCecLinkOpener {
        async fn open(
            &self,
            device: &str,
            _physical_address: PhysicalAddress,
        ) -> Result<Box<dyn CecLink>> {
            if self.deny_permission {
                return Err(Error::PermissionDenied {
                    path: device.to_string(),
                });
            }
            let (osd_name, vendor_id) = self.identity.clone().unwrap_or_default();
            Ok(Box::new(FakeCecLink {
                power: Mutex::new(self.initial),
                ignore_wakes: Mutex::new(self.ignore_wakes),
                osd_name,
                vendor_id,
            }))
        }

        async fn open_monitor(&self, _device: &str) -> Result<Box<dyn CecMonitor>> {
            let script = self.monitor_script.lock().take().unwrap_or_default();
            Ok(Box::new(FakeCecMonitor {
                script: Mutex::new(script),
                fails: self.monitor_fails,
            }))
        }
    }

    /// Replays a fixed script of inbound (initiator, message) pairs, then
    /// idles (yielding so the subscription task can notice its consumer
    /// went away) — or, when `fails`, returns `ConnectionClosed` once the
    /// script drains, modeling an adapter disconnect.
    #[derive(Debug)]
    pub(crate) struct FakeCecMonitor {
        script: Mutex<VecDeque<(LogicalAddress, Message)>>,
        fails: bool,
    }

    #[async_trait]
    impl CecMonitor for FakeCecMonitor {
        async fn recv(&self) -> Result<Option<(LogicalAddress, Message)>> {
            let next = self.script.lock().pop_front();
            match next {
                Some(pair) => Ok(Some(pair)),
                None if self.fails => Err(Error::ConnectionClosed),
                None => {
                    tokio::task::yield_now().await;
                    Ok(None)
                }
            }
        }
    }
}
