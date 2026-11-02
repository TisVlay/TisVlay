//! The CEC transport seam.
//!
//! `linux-cec` ships no fake server, so this trait is tisvlay-cec's
//! testability boundary: production wraps [`linux_cec::device::AsyncDevice`],
//! and tests use a scripted `FakeCecLink`. It deliberately sits one layer
//! lower than LG's `FakeSsapServer` (which fakes the *peer* and drives the
//! real transport) — here we fake the *library wrapper*, so unit tests
//! never touch a real `/dev/cecN` ioctl. Daemon-level coverage of the
//! always-reachable state machine rides the mock backend, not this seam.

// External-input profile: this module handles CEC frames from the kernel,
// so indexing, arithmetic, string slicing and narrowing casts must be
// checked here (docs/build.md, Lint policy).
#![warn(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::cast_possible_truncation
)]

use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

use async_trait::async_trait;
use linux_cec::device::{
    AsyncDevice, Capabilities as CecCapabilities, LogicalAddressFlags, MessageData,
};
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
    /// Open a tx handle for `device`, with the adapter holding a Playback
    /// logical address. `claim` says whether this open takes the claim over
    /// (and with which OSD name) or reuses one the adapter already holds.
    async fn open(
        &self,
        device: &str,
        physical_address: PhysicalAddress,
        claim: Claim<'_>,
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

    /// Whether the adapter can hand the remote's keys to the kernel's RC
    /// device (`CEC_CAP_RC`), which [`set_rc_passthrough`](Self::set_rc_passthrough)
    /// needs.
    fn has_rc(&self) -> bool;
    /// Allow or stop RC passthrough, so the kernel forwards the TV remote's
    /// keys to the adapter's RC device. A change re-claims the held logical
    /// address with the flag, keeping its OSD name; a claim that already
    /// matches is left alone.
    async fn set_rc_passthrough(&self, allow: bool) -> Result<()>;
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

/// What an open does with the adapter's logical-address claim.
///
/// The kernel keeps a claim, and the OSD name it carries, after every handle
/// closes, so an open either takes the claim over or leaves it alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Claim<'a> {
    /// Claim afresh, advertising this OSD name (`None`: no name): the label
    /// the TV shows for the PC's HDMI input. The connection-holding opens
    /// (`connect`, `wake_and_connect`) own the label and use this.
    Advertise(Option<&'a str>),
    /// Reuse a held claim and its name as they are, and claim without a
    /// name only when the adapter holds none. The bare wake and the
    /// discovery probe use this, so they never change the label a
    /// connection advertised.
    Keep,
}

/// The steps an open takes on the adapter, from its [`Claim`] and whether
/// the adapter already holds a logical address.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClaimPlan {
    /// Use the held claim untouched.
    Reuse,
    /// Clear the held claim when `clear`, set `name`, then claim.
    Claim { clear: bool, name: Option<String> },
}

impl ClaimPlan {
    /// Plan an open. The name is clamped here (14-byte CEC field, empty →
    /// nothing) as idempotent defense: config normalizes at parse, but this
    /// seam takes any `&str`.
    ///
    /// **Load-bearing order:** a held claim is cleared *before* the name is
    /// set. On a clear the kernel blanks its stored name and copies its
    /// config back over linux-cec's, so a name set first is lost. That is
    /// what happens when `set_logical_address` clears a held claim itself,
    /// and the kernel then refuses the TV's `<Give OSD Name>`, so the input
    /// reads "HDMI 1".
    pub(crate) fn new(claim: Claim<'_>, claimed: bool) -> Self {
        match claim {
            Claim::Keep if claimed => Self::Reuse,
            Claim::Keep => Self::Claim {
                clear: false,
                name: None,
            },
            Claim::Advertise(name) => Self::Claim {
                clear: claimed,
                name: name.and_then(crate::identity::clamp_osd_name),
            },
        }
    }
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
        claim: Claim<'_>,
    ) -> Result<Box<dyn CecLink>> {
        Ok(Box::new(
            LinuxCecLink::open(device, physical_address, claim).await?,
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
    /// The `/dev/cecN` path, for logging a re-claim.
    path: String,
    /// The adapter has `CEC_CAP_RC`.
    rc: bool,
}

impl LinuxCecLink {
    /// Claim a Playback address with `flags`, on an adapter whose claim was
    /// just cleared.
    async fn claim_with(&self, flags: LogicalAddressFlags) -> Result<()> {
        self.device
            .set_logical_address_flags(flags)
            .await
            .map_err(map_cec_err)?;
        self.device
            .set_logical_address(LogicalAddressType::Playback)
            .await
            .map_err(map_cec_err)
    }

    async fn open(
        device: &str,
        physical_address: PhysicalAddress,
        claim: Claim<'_>,
    ) -> Result<Self> {
        let dev = AsyncDevice::open(device)
            .await
            .map_err(|e| map_open_err(device, e))?;
        let claimed = !dev
            .get_logical_addresses()
            .await
            .map_err(map_cec_err)?
            .is_empty();
        let capabilities = dev.get_capabilities().await.map_err(map_cec_err)?;
        let link = |handle| Self {
            device: handle,
            path: device.to_owned(),
            rc: capabilities.contains(CecCapabilities::RC),
        };
        let ClaimPlan::Claim { clear, name } = ClaimPlan::new(claim, claimed) else {
            return Ok(link(dev));
        };
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
        if capabilities.contains(CecCapabilities::PHYS_ADDR) {
            dev.set_physical_address(physical_address)
                .await
                .map_err(map_cec_err)?;
        }
        if clear {
            dev.clear_logical_addresses().await.map_err(map_cec_err)?;
        }
        // Advertise our OSD name before claiming the logical address, so the
        // name rides the same log-address configuration the kernel then
        // auto-answers <Give OSD Name> from. Set while unregistered, so this
        // only writes the local config: no frame leaves before we are on the
        // bus. Best-effort by design: the name is cosmetic, so a failure
        // here logs and continues rather than costing the caller the whole
        // connection (and with it all power/screen control).
        let mut advertised = None;
        if let Some(name) = name {
            match dev.set_osd_name(&name).await {
                Ok(()) => advertised = Some(name),
                Err(error) => tracing::warn!(
                    device,
                    osd_name = %name,
                    %error,
                    "failed to advertise CEC OSD name; continuing without it"
                ),
            }
        }
        // Claim a Playback logical address so the adapter is allowed to
        // transmit on the bus.
        dev.set_logical_address(LogicalAddressType::Playback)
            .await
            .map_err(map_cec_err)?;
        match dev.get_logical_addresses().await {
            Ok(addresses) => log_claim(device, addresses.first().copied(), advertised.as_deref()),
            Err(error) => tracing::debug!(device, %error, "couldn't read back the CEC claim"),
        }
        Ok(link(dev))
    }
}

/// Log a claim's outcome: the address the kernel picked and the name it
/// now serves. A Backup or Unregistered address warns: a CEC 1.4 TV has
/// no Backup addresses and doesn't name an unregistered device, so the
/// input label may not follow.
fn log_claim(device: &str, address: Option<LogicalAddress>, osd_name: Option<&str>) {
    let osd_name = osd_name.unwrap_or("none");
    match address {
        Some(
            address @ (LogicalAddress::Backup1
            | LogicalAddress::Backup2
            | LogicalAddress::UnregisteredOrBroadcast),
        ) => tracing::warn!(
            device,
            logical_address = u8::from(address),
            %address,
            osd_name,
            "claimed a CEC logical address the TV may not name"
        ),
        Some(address) => tracing::info!(
            device,
            logical_address = u8::from(address),
            %address,
            osd_name,
            "claimed CEC logical address"
        ),
        None => tracing::warn!(
            device,
            osd_name,
            "CEC claim ended without a logical address"
        ),
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

    fn has_rc(&self) -> bool {
        self.rc
    }

    async fn set_rc_passthrough(&self, allow: bool) -> Result<()> {
        let mut flags = self
            .device
            .get_logical_address_flags()
            .await
            .map_err(map_cec_err)?;
        if flags.contains(LogicalAddressFlags::ALLOW_RC_PASSTHRU) == allow {
            return Ok(());
        }
        let held = flags;
        flags.set(LogicalAddressFlags::ALLOW_RC_PASSTHRU, allow);
        // Our own name, as the claim holds it: a 14-byte NUL-padded buffer.
        let name = self.device.get_osd_name().await.map_err(map_cec_err)?;
        let name = name
            .as_bytes()
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        let name = crate::identity::clamp_osd_name(&String::from_utf8_lossy(name));
        // The order `ClaimPlan` documents: clearing blanks the stored name
        // and flags, so both go back on after it, then the claim.
        self.device
            .clear_logical_addresses()
            .await
            .map_err(map_cec_err)?;
        if let Some(name) = &name
            && let Err(error) = self.device.set_osd_name(name).await
        {
            tracing::warn!(device = self.path, osd_name = %name, %error, "failed to keep the CEC OSD name across a re-claim");
        }
        if let Err(error) = self.claim_with(flags).await {
            // The clear already took the PC off the bus: claim again with
            // the flags it held, so a failed change costs the passthrough
            // change only. The next connect's claim applies it again.
            if let Err(fallback) = self.claim_with(held).await {
                tracing::warn!(device = self.path, error = %fallback, "couldn't restore the CEC claim after a failed re-claim");
            }
            return Err(error);
        }
        let address = self
            .device
            .get_logical_addresses()
            .await
            .ok()
            .and_then(|addresses| addresses.first().copied());
        tracing::info!(
            device = self.path,
            rc_passthrough = allow,
            "re-claimed for RC passthrough"
        );
        log_claim(&self.path, address, name.as_deref());
        Ok(())
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

#[cfg(any(test, feature = "test-link"))]
pub use fake::{FakeCecLink, FakeCecLinkOpener};

#[cfg(any(test, feature = "test-link"))]
mod fake {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    /// A scripted `CecLink` for tests: holds a mutable power status that
    /// `power_on` / `power_off` flip, so connector and connection logic
    /// run without a real `/dev/cecN`. `ignore_wakes` models a TV that
    /// drops the first N `power_on` sends before reporting on, so tests
    /// can exercise the wake re-send loop.
    #[derive(Debug)]
    pub struct FakeCecLink {
        power: Mutex<PowerStatus>,
        ignore_wakes: Mutex<usize>,
        /// Scripted identity for `query_osd_name` / `query_vendor_id`.
        /// Defaults are empty/`None` (no identity) so existing tests, which
        /// don't assert identity, see an all-absent result.
        osd_name: String,
        vendor_id: Option<linux_cec::VendorId>,
        /// The adapter has `CEC_CAP_RC` (default: it does).
        rc: bool,
    }

    impl FakeCecLink {
        /// A fake link reporting `power`, with no scripted identity and
        /// honoring every wake.
        pub fn new(power: PowerStatus) -> Self {
            Self {
                power: Mutex::new(power),
                ignore_wakes: Mutex::new(0),
                osd_name: String::new(),
                vendor_id: None,
                rc: true,
            }
        }

        /// A TV that drops the first `ignored` Image View On sends before
        /// it wakes (`usize::MAX`: one that never does).
        #[must_use]
        pub fn with_ignored_wakes(self, ignored: usize) -> Self {
            *self.ignore_wakes.lock() = ignored;
            self
        }

        /// An adapter without `CEC_CAP_RC`.
        #[must_use]
        pub fn without_rc(mut self) -> Self {
            self.rc = false;
            self
        }

        /// Seed the scripted TV identity the connection/discovery queries.
        #[must_use]
        pub fn with_identity(
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
                *remaining = remaining.saturating_sub(1);
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

        fn has_rc(&self) -> bool {
            self.rc
        }

        async fn set_rc_passthrough(&self, _allow: bool) -> Result<()> {
            Ok(())
        }
    }

    /// Opener that yields a [`FakeCecLink`] seeded with a fixed initial
    /// power status, regardless of the requested device path.
    #[derive(Debug)]
    pub struct FakeCecLinkOpener {
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
        /// Records, per `open` call in order, the name the production link
        /// would actually advertise — the *clamped* value, not the raw
        /// argument — so tests observe the advertised bytes and clamp
        /// regressions surface in the advertise assertions.
        opened_with: Mutex<Vec<Option<String>>>,
        /// The adapter's claim as the kernel holds it across opens: `None`
        /// when unclaimed, else the OSD name it answers the TV with.
        adapter: Mutex<Option<Option<String>>>,
    }

    impl FakeCecLinkOpener {
        /// An opener whose links start at `initial` power, with nothing
        /// scripted and every open permitted.
        pub fn new(initial: PowerStatus) -> Self {
            Self {
                initial,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: None,
                deny_permission: false,
                opened_with: Mutex::new(Vec::new()),
                adapter: Mutex::new(None),
            }
        }

        /// Opener yielding a standby TV that only wakes after `ignored`
        /// dropped `power_on` sends — exercises the wake re-send loop.
        pub fn new_flaky_wake(ignored: usize) -> Self {
            Self {
                initial: PowerStatus::Standby,
                ignore_wakes: ignored,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: None,
                deny_permission: false,
                opened_with: Mutex::new(Vec::new()),
                adapter: Mutex::new(None),
            }
        }

        /// Opener whose monitor handle replays `script` (one message per
        /// `recv`) as if each came from the TV, then idles — drives
        /// `subscribe_state` tests.
        pub fn with_monitor_script(script: Vec<Message>) -> Self {
            Self::with_monitor_script_from(
                script
                    .into_iter()
                    .map(|message| (LogicalAddress::Tv, message))
                    .collect(),
            )
        }

        /// Like [`Self::with_monitor_script`] but with explicit initiators,
        /// so tests can exercise the monitor's initiator filter.
        pub fn with_monitor_script_from(script: Vec<(LogicalAddress, Message)>) -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(Some(script.into())),
                monitor_fails: false,
                identity: None,
                deny_permission: false,
                opened_with: Mutex::new(Vec::new()),
                adapter: Mutex::new(None),
            }
        }

        /// Opener whose opened links report the given TV identity — for the
        /// discovery-probe tests.
        pub fn with_identity(osd_name: &str, vendor_id: Option<linux_cec::VendorId>) -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: Some((osd_name.to_string(), vendor_id)),
                deny_permission: false,
                opened_with: Mutex::new(Vec::new()),
                adapter: Mutex::new(None),
            }
        }

        /// Opener whose `open` fails with [`Error::PermissionDenied`],
        /// modeling a node the daemon's user can enumerate but not open.
        pub fn permission_denied() -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: false,
                identity: None,
                deny_permission: true,
                opened_with: Mutex::new(Vec::new()),
                adapter: Mutex::new(None),
            }
        }

        /// Opener whose monitor `recv` returns `ConnectionClosed` — models a
        /// real adapter disconnect, so tests can assert the monitor flips
        /// `is_connected()`.
        pub fn with_monitor_disconnect() -> Self {
            Self {
                initial: PowerStatus::On,
                ignore_wakes: 0,
                monitor_script: Mutex::new(None),
                monitor_fails: true,
                identity: None,
                deny_permission: false,
                opened_with: Mutex::new(Vec::new()),
                adapter: Mutex::new(None),
            }
        }
        /// The OSD name each `open` call would advertise (post-clamp), in
        /// order — `None` for an open that advertises nothing.
        pub fn opened_osd_names(&self) -> Vec<Option<String>> {
            self.opened_with.lock().clone()
        }

        /// The OSD name the adapter answers the TV's `<Give OSD Name>`
        /// with after every open so far — `None` when unclaimed or claimed
        /// without a name (the TV then shows its own input label).
        pub fn adapter_osd_name(&self) -> Option<String> {
            self.adapter.lock().clone().flatten()
        }
    }

    #[async_trait]
    impl CecLinkOpener for FakeCecLinkOpener {
        async fn open(
            &self,
            device: &str,
            _physical_address: PhysicalAddress,
            claim: Claim<'_>,
        ) -> Result<Box<dyn CecLink>> {
            let mut adapter = self.adapter.lock();
            let plan = ClaimPlan::new(claim, adapter.is_some());
            // Record through the same plan production follows, so the
            // assertions see the clamped, on-bus bytes.
            self.opened_with.lock().push(match &plan {
                ClaimPlan::Reuse => None,
                ClaimPlan::Claim { name, .. } => name.clone(),
            });
            if self.deny_permission {
                return Err(Error::PermissionDenied {
                    path: device.to_string(),
                });
            }
            if let ClaimPlan::Claim { clear, name } = plan {
                if clear {
                    *adapter = None;
                }
                // `set_logical_address` clears a held claim itself first,
                // and the kernel blanks the name we set on that clear.
                *adapter = Some(if adapter.is_some() { None } else { name });
            }
            drop(adapter);
            let (osd_name, vendor_id) = self.identity.clone().unwrap_or_default();
            Ok(Box::new(FakeCecLink {
                power: Mutex::new(self.initial),
                ignore_wakes: Mutex::new(self.ignore_wakes),
                osd_name,
                vendor_id,
                rc: true,
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
