//! Pairing state machine for the mock backend.
//!
//! Pairing is modelled as an explicit state transition
//! `Idle → Prompted → {Accepted, Rejected, TimedOut}`. Real backends
//! have much more complex flows (WebSocket handshake, capability
//! exchange, credential persistence, etc.), but from the tisvlay
//! daemon's perspective the only thing that matters is the outcome:
//! did the user accept, reject, or time out.
//!
//! Tests drive the outcome via [`MockHandle`](crate::MockHandle),
//! which writes into [`MockState::pairing_outcomes`](crate::state::MockState).
//! The connector's `start_pairing()` implementation consumes the
//! outcome (if one has been seeded) and drives the [`PairingSession`]
//! state channel accordingly. Simple tests use a config-level
//! shorthand (`pairing = "accept"`) that pre-seeds an outcome before
//! the test runs, so they don't have to touch the handle at all.

use serde::{Deserialize, Serialize};

/// Pre-seeded outcome for a pairing session.
///
/// Stored in [`MockState::pairing_outcomes`](crate::state::MockState)
/// and consumed by `MockConnector::start_pairing()` when the session
/// is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingOutcome {
    /// Accept the pairing request, returning a dummy client key.
    Accept,
    /// Reject the pairing request (simulates the user declining
    /// on the TV remote).
    Reject,
    /// Simulate a pairing timeout (user never responded).
    Timeout,
}

/// Vendor-config shorthand for the common pairing scenarios.
///
/// Surfaced in `[tv.mock] pairing = "..."` and parsed by serde.
/// The shorthand pre-seeds a [`PairingOutcome`] before the test
/// starts, so simple tests don't need to drive the handle manually.
///
/// Tests that need fine-grained control (e.g., a mix of accept and
/// reject scenarios within one test) use the handle directly and
/// leave this at the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PairingShorthand {
    /// Leave pairing control to the test handle; no pre-seeded outcome.
    /// (Default if the config key is absent.)
    #[default]
    Manual,
    /// Auto-accept pairing when `start_pairing()` is called.
    Accept,
    /// Auto-reject pairing when `start_pairing()` is called.
    Reject,
    /// Simulate a timeout when `start_pairing()` is called.
    Timeout,
}

impl PairingShorthand {
    /// Convert the shorthand into a pre-seeded outcome, if any.
    pub(crate) fn to_outcome(self) -> Option<PairingOutcome> {
        match self {
            Self::Manual => None,
            Self::Accept => Some(PairingOutcome::Accept),
            Self::Reject => Some(PairingOutcome::Reject),
            Self::Timeout => Some(PairingOutcome::Timeout),
        }
    }
}
