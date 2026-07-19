//! Shared supervise cadence for the remote-source detectors.
//!
//! Each detector (logind, portal monitor, PipeWire screencast) runs a
//! supervised loop that connects to its transport, follows it until it
//! ends, then backs off and retries. The retry cadence and the log-noise
//! discipline are identical across all three, so they live here; each
//! detector keeps only its per-transport body. Works for both the async
//! (tokio) and the sync (OS-thread) supervisors — [`Backoff::next_delay`]
//! just hands back a `Duration` for the caller to sleep on.

use std::time::Duration;

use tracing::{debug, warn};

/// Why a supervised watch session ended. Shared across the detectors so
/// the supervise cadence and its fault taxonomy live in one place.
pub(crate) enum Fault {
    /// The transport could not be reached or faulted — retry under backoff.
    Bus(zbus::Error),
    /// The session core dropped the detector's channel — stop supervising.
    CoreGone,
}

impl From<zbus::Error> for Fault {
    fn from(e: zbus::Error) -> Self {
        Fault::Bus(e)
    }
}

/// Backoff bounds for the supervised detector loops.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Doubling-capped backoff with a warn-once-then-debug failure policy: the
/// first connect failure warns (a subsystem may be genuinely absent), the
/// rest are debug, so a box without the transport does not spam the log.
pub(crate) struct Backoff {
    delay: Duration,
    warned: bool,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            delay: RETRY_MIN,
            warned: false,
        }
    }
}

impl Backoff {
    /// A watch session ended cleanly (the connection dropped): reset the
    /// delay so a returning server reconnects promptly. The warn latch
    /// stays set — a later failure after one success is still debug.
    pub(crate) fn reset(&mut self) {
        self.delay = RETRY_MIN;
    }

    /// Record a connect/transport failure under `subsystem`: warn on the
    /// first, debug thereafter.
    pub(crate) fn note_failure(&mut self, subsystem: &str, error: impl std::fmt::Display) {
        if !self.warned {
            warn!(%error, "{subsystem} unavailable; retrying quietly");
            self.warned = true;
        } else {
            debug!(%error, "{subsystem} connect failed");
        }
    }

    /// The delay to sleep before the next attempt, then double-and-cap for
    /// the one after.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.delay;
        self.delay = (self.delay * 2).min(RETRY_MAX);
        delay
    }
}
