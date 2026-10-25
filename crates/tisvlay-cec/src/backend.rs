//! `CecBackend` — the trait metadata container.
//!
//! Like every backend, this is a thin metadata trait; the behavior lives
//! on [`CecConnector`](crate::CecConnector) and
//! [`CecConnection`](crate::CecConnection).

use tisvlay_core::{Capabilities, TvBackend};

/// CEC backend metadata.
#[derive(Debug)]
#[non_exhaustive]
pub struct CecBackend;

impl CecBackend {
    /// Create a new CEC backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CecBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TvBackend for CecBackend {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn vendor_name(&self) -> &'static str {
        crate::VENDOR_NAME
    }

    fn capabilities(&self) -> Capabilities {
        // Power on/off + power-status readback, and "discovery" by enumerating
        // the host's `/dev/cecN` nodes. INPUT is deliberately *not* advertised:
        // CEC's only reliable input op is "make the TV show our own port"
        // (`Active Source`, already sent at wake); routing to an arbitrary
        // other input relies on `Set Stream Path` reaching a CEC device that
        // answers at the target port, which is TV-dependent and unreliable in
        // practice. The `switch_input` impl is kept dormant behind this bit.
        // Volume over CEC is relative-only (deferred behind a future
        // capability); backlight / OLED screen-blank aren't CEC concepts.
        // Declaring neither IPV4 nor IPV6 is load-bearing beyond "not a network
        // transport": it stops the daemon's manual-entry enrichment from
        // ARP-probing a `/dev/cecN` path as if it were an IP address.
        // REMOTE_POWER: the TV's "power off this device" reaches every
        // connection's monitor.
        Capabilities::POWER | Capabilities::DISCOVERY | Capabilities::REMOTE_POWER
    }
}
