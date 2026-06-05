//! Epson projector backend metadata.
//!
//! [`TvBackend`] is a thin trait reporting vendor identity,
//! capabilities, and the reliable-dispatch tuning. The control
//! behavior lives on [`EpsonConnector`](crate::connector::EpsonConnector)
//! and [`EpsonConnection`](crate::connection::EpsonConnection).

use std::time::Duration;
use tisvlay_core::{Capabilities, RetryConfig, TvBackend};

/// Epson projector backend.
#[derive(Debug, Clone, Default)]
pub struct EpsonBackend;

impl EpsonBackend {
    /// Create a new Epson backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl TvBackend for EpsonBackend {
    fn vendor_id(&self) -> &'static str {
        crate::VENDOR_ID
    }

    fn vendor_name(&self) -> &'static str {
        crate::VENDOR_NAME
    }

    fn capabilities(&self) -> Capabilities {
        // A projector is speakerless (audio routes through the AVR) and
        // ESC/VP21 has no volume command, so VOLUME and MUTE are out —
        // A/V mute IS the SCREEN feature (blank the image to save laser
        // hours). BACKLIGHT maps to LUMLEVEL (laser brightness).
        // DISCOVERY is deferred to a hardware-validation follow-up.
        // Only IPV4 is declared (v6 unverified on the projector).
        Capabilities::POWER
            | Capabilities::SCREEN
            | Capabilities::INPUT
            | Capabilities::BACKLIGHT
            | Capabilities::WOL
            | Capabilities::IPV4
    }

    fn verify_retry_config(&self) -> RetryConfig {
        // A projector's state read is a polled ESC/VP21 round-trip, not
        // LG's sub-100 ms push, so the post-command verify needs a
        // wider window for the A/V-mute transition to land and the next
        // `MUTE?` poll to reflect it. One second is comfortably above a
        // TCP round-trip without making a failed `screen_off` feel
        // sluggish; two attempts (the default count) cover a single
        // missed poll. Tune from measured timings during hardware
        // validation.
        RetryConfig {
            verify_delay: Duration::from_secs(1),
            max_attempts: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_constants() {
        let backend = EpsonBackend::new();
        assert_eq!(backend.vendor_id(), "epson");
        assert_eq!(backend.vendor_name(), "Epson Projector");
    }

    #[test]
    fn capabilities_match_projector_shape() {
        let caps = EpsonBackend::new().capabilities();
        assert!(caps.contains(Capabilities::POWER));
        assert!(caps.contains(Capabilities::SCREEN));
        assert!(caps.contains(Capabilities::INPUT));
        assert!(caps.contains(Capabilities::BACKLIGHT));
        assert!(caps.contains(Capabilities::WOL));
        assert!(caps.contains(Capabilities::IPV4));
        // A projector is speakerless and has no discovery in v1.
        assert!(!caps.contains(Capabilities::VOLUME));
        assert!(!caps.contains(Capabilities::MUTE));
        assert!(!caps.contains(Capabilities::DISCOVERY));
        assert!(!caps.contains(Capabilities::IPV6));
    }

    #[test]
    fn verify_retry_is_slower_than_lg_default() {
        let cfg = EpsonBackend::new().verify_retry_config();
        assert!(
            cfg.verify_delay > RetryConfig::default().verify_delay,
            "projector verify delay should exceed LG's push-tuned default",
        );
        assert_eq!(cfg.max_attempts, 2);
    }
}
