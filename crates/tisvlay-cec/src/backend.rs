//! `CecBackend` — the trait metadata container.
//!
//! Like every backend, this is a thin metadata trait; the behavior lives
//! on [`CecConnector`](crate::CecConnector) and
//! [`CecConnection`](crate::CecConnection).

use tisvlay_core::{Capabilities, TvBackend};

/// CEC backend metadata.
#[derive(Debug)]
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
        // CEC reliably does power on/off + power-status readback, and can
        // switch the TV to a given HDMI input (`Set Stream Path`). Volume
        // over CEC is relative-only (deferred behind a future capability),
        // and backlight / OLED screen-blank aren't CEC concepts. Declaring
        // neither IPV4 nor IPV6 is load-bearing beyond "not a network
        // transport": it stops the daemon's manual-entry enrichment from
        // ARP-probing a `/dev/cecN` path as if it were an IP address.
        Capabilities::POWER | Capabilities::INPUT
    }
}
