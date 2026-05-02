//! Typed errors for the power-cycle path (wake / connect).
//!
//! Each variant covers a distinct failure mode the daemon can hit
//! while bringing a TV up, with enough payload for clients (GUI,
//! tray, CLI) to surface a meaningful message — and a stable
//! machine-readable [`PowerErrorCode`] for routing.
//!
//! `tv_id` is intentionally NOT a field on these variants: the
//! caller already knows which TV it acted on, and including it
//! everywhere would be redundant in log output. Wrapping enums
//! (`TvManagerError`, `DaemonError`) carry `tv_id` once at the
//! outer layer.

use std::fmt;

/// Stable machine-readable code for a [`PowerError`]. Matches the
/// kebab-case convention used elsewhere in the project (e.g. the
/// pairing-error codes' Display impl).
///
/// Clients route on this value — e.g. the GUI's inline-error
/// surface checks `err.code == PowerErrorCode::NoMacConfigured`
/// to decide whether to offer an "Edit MAC" recovery action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerErrorCode {
    /// TV has no wake method configured.
    NoMacConfigured,
    /// Wake-on-LAN packet send failed at the network layer.
    WolSendFailed,
    /// WoL was sent but the TV didn't come up within the wait window.
    WakeTimeout,
    /// TV reachable on the network but not accepting our connection
    /// (firewall, wrong port, daemon hasn't booted the protocol layer
    /// yet, etc.).
    ConnectTimeout,
    /// TV is already powered on; the requested operation is a no-op.
    AlreadyOn,
    /// TV is already powered off; the requested operation is a no-op.
    AlreadyOff,
}

impl PowerErrorCode {
    /// Stable identifier for routing on clients. Format mirrors the
    /// `<domain>.<kebab-case-variant>` convention used throughout
    /// freedesktop ecosystem error vocabularies.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoMacConfigured => "power.no-mac-configured",
            Self::WolSendFailed => "power.wol-send-failed",
            Self::WakeTimeout => "power.wake-timeout",
            Self::ConnectTimeout => "power.connect-timeout",
            Self::AlreadyOn => "power.already-on",
            Self::AlreadyOff => "power.already-off",
        }
    }
}

impl fmt::Display for PowerErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed power-cycle error. Each variant carries the data
/// specific to that failure mode (e.g. `WakeTimeout` carries the
/// elapsed wait, `WolSendFailed` carries the underlying I/O error
/// as a string for cross-process/D-Bus serializability).
#[derive(Debug, Clone, thiserror::Error)]
pub enum PowerError {
    /// TV has no wake method configured. For LG WebOS this means
    /// the `[tv.lg]` config block is missing a `mac` field, so we
    /// can't send Wake-on-LAN. Recovery: re-pair the TV (so the
    /// pairing flow can capture MAC via ARP) or edit the config
    /// to add the MAC manually.
    #[error("no wake method configured")]
    NoMacConfigured,

    /// Wake-on-LAN packet send failed at the network layer.
    /// Common causes: no route to the broadcast address (laptop
    /// suspended its wifi), firewall dropping UDP/9, NIC bonding
    /// in transition.
    #[error("Wake-on-LAN send failed: {message}")]
    WolSendFailed {
        /// Stringified underlying error. Stored as `String` rather
        /// than `io::Error` because `PowerError` is `Clone` and
        /// crosses D-Bus. Field named `message` (not `source`) so
        /// `thiserror` doesn't treat it as the `std::error::Error`
        /// source — `String` doesn't impl `Error`.
        message: String,
    },

    /// Sent WoL but the TV didn't come up within the wait window.
    /// Check that the TV has "TV On via Wi-Fi" / "Mobile TV On"
    /// enabled in its network settings; some LG firmwares ignore
    /// WoL during pixel-refresh cycles.
    #[error("Wake-on-LAN sent but TV did not respond within {timeout_ms}ms")]
    WakeTimeout {
        /// How long we waited, in milliseconds.
        timeout_ms: u32,
    },

    /// TV reachable on the network but not accepting our
    /// connection. Distinct from `WakeTimeout`: WoL succeeded (or
    /// wasn't needed), the TV is up, but the protocol-level
    /// connection (WebSocket for LG) doesn't establish.
    #[error("connection to TV timed out after {timeout_ms}ms")]
    ConnectTimeout {
        /// How long we waited, in milliseconds.
        timeout_ms: u32,
    },

    /// `power_on` called but TV is already on. Idempotency guard;
    /// not a user-visible failure unless the GUI surfaces it.
    #[error("TV is already powered on")]
    AlreadyOn,

    /// `power_off` called but TV is already off. Idempotency guard.
    #[error("TV is already powered off")]
    AlreadyOff,
}

impl PowerError {
    /// Stable machine-readable identifier — see [`PowerErrorCode`].
    pub const fn code(&self) -> PowerErrorCode {
        match self {
            Self::NoMacConfigured => PowerErrorCode::NoMacConfigured,
            Self::WolSendFailed { .. } => PowerErrorCode::WolSendFailed,
            Self::WakeTimeout { .. } => PowerErrorCode::WakeTimeout,
            Self::ConnectTimeout { .. } => PowerErrorCode::ConnectTimeout,
            Self::AlreadyOn => PowerErrorCode::AlreadyOn,
            Self::AlreadyOff => PowerErrorCode::AlreadyOff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trip() {
        assert_eq!(
            PowerError::NoMacConfigured.code(),
            PowerErrorCode::NoMacConfigured,
        );
        assert_eq!(
            PowerError::WakeTimeout { timeout_ms: 8000 }.code(),
            PowerErrorCode::WakeTimeout,
        );
    }

    #[test]
    fn code_strings_are_kebab_case() {
        assert_eq!(
            PowerErrorCode::NoMacConfigured.as_str(),
            "power.no-mac-configured",
        );
        assert_eq!(
            PowerErrorCode::WolSendFailed.as_str(),
            "power.wol-send-failed",
        );
    }

    #[test]
    fn display_includes_payload() {
        let e = PowerError::WakeTimeout { timeout_ms: 8000 };
        let s = e.to_string();
        assert!(s.contains("8000"));
        assert!(s.contains("Wake-on-LAN"));
    }
}
