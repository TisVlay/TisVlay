//! The rpc error contract: every `reply_err` payload is this shape,
//! clients branch on `code` and show `message`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Declare the rpc-reply error codes: one line per code generates the
/// enum (serialized as the kebab-case string), `as_str` and `ALL`.
macro_rules! error_codes {
        ($( $(#[$meta:meta])* $name:ident = $code:literal, )*) => {
            /// Why a service refused an rpc: the `code` of every
            /// [`WireError`]. Serialized as its kebab-case string.
            #[non_exhaustive]
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
            pub enum ErrorCode {
                $( $(#[$meta])* #[serde(rename = $code)] $name, )*
            }

            impl ErrorCode {
                /// Every code, in declaration order.
                pub const ALL: &[Self] = &[$(Self::$name),*];

                /// The code as it travels on the wire.
                pub const fn as_str(self) -> &'static str {
                    match self {
                        $( Self::$name => $code, )*
                    }
                }
            }
        };
    }

error_codes! {
    TvNotFound = "tv-not-found",
    TvExists = "tv-exists",
    NotConnected = "not-connected",
    UnknownVendor = "unknown-vendor",
    PairingExists = "pairing-exists",
    PairingNotFound = "pairing-not-found",
    WakeNotSupported = "wake-not-supported",
    BadRequest = "bad-request",
    /// A vendor field failed validation (see [`WireError::invalid_field`]).
    InvalidField = "invalid-field",
    ProfileReserved = "profile-reserved",
    /// The TV refused or failed the command.
    CommandFailed = "command-failed",
    ProfileNotFound = "profile-not-found",
    ProfileExists = "profile-exists",
    ProfileInUse = "profile-in-use",
    UnknownRpc = "unknown-rpc",
    DiscoveryNotFound = "discovery-not-found",
    DaemonShuttingDown = "daemon-shutting-down",
    /// The service runs without a state file, so there's nothing to
    /// reload.
    NoStore = "no-store",
    /// The state file couldn't be read or parsed; a reload applies
    /// nothing.
    StoreUnreadable = "store-unreadable",
}

/// Split a [`WireError::invalid_field`] refusal back into its
/// `(field, reason)`; `None` for any other code.
pub fn invalid_field_parts(code: ErrorCode, message: &str) -> Option<(&str, &str)> {
    if code != ErrorCode::InvalidField {
        return None;
    }
    let (field, reason) = message.split_once(": ")?;
    (!field.is_empty()).then_some((field, reason))
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed failure reason for a command reply — serialized into the
/// zenoh `reply_err` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
}

impl WireError {
    pub fn tv_not_found(id: Uuid) -> Self {
        Self {
            code: ErrorCode::TvNotFound,
            message: format!("no TV with id {id}"),
        }
    }

    pub fn tv_exists(id: Uuid) -> Self {
        Self {
            code: ErrorCode::TvExists,
            message: format!("a TV with id {id} already exists"),
        }
    }

    pub fn not_connected(id: Uuid) -> Self {
        Self {
            code: ErrorCode::NotConnected,
            message: format!("TV {id} has no live connection"),
        }
    }

    pub fn unknown_vendor(vendor: &str) -> Self {
        Self {
            code: ErrorCode::UnknownVendor,
            message: format!("no backend registered for vendor '{vendor}'"),
        }
    }

    pub fn pairing_exists(id: Uuid) -> Self {
        Self {
            code: ErrorCode::PairingExists,
            message: format!("a pairing session for {id} is already running"),
        }
    }

    pub fn pairing_not_found(id: Uuid) -> Self {
        Self {
            code: ErrorCode::PairingNotFound,
            message: format!("no pairing session for {id}"),
        }
    }

    pub fn wake_not_supported(id: Uuid) -> Self {
        Self {
            code: ErrorCode::WakeNotSupported,
            message: format!("TV {id} is disconnected and its transport cannot wake it"),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::BadRequest,
            message: message.into(),
        }
    }

    /// A specific vendor-config field failed validation. The
    /// message's `"<field>: <reason>"` shape is load-bearing:
    /// clients route the reason back to the originating form row
    /// ([`invalid_field_parts`]) by splitting on the first `": "`,
    /// since a reason may contain colons (an IPv6 address).
    pub fn invalid_field(field: &str, reason: impl std::fmt::Display) -> Self {
        Self {
            code: ErrorCode::InvalidField,
            message: format!("{field}: {reason}"),
        }
    }

    /// The reserved default profile cannot be deleted.
    pub fn profile_reserved(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileReserved,
            message: format!("profile {id} is the reserved default and cannot be deleted"),
        }
    }

    /// The backend accepted the command but the device call failed.
    pub fn command_failed(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::CommandFailed,
            message: message.into(),
        }
    }

    pub fn profile_not_found(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileNotFound,
            message: format!("no system profile with id {id}"),
        }
    }

    pub fn profile_exists(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileExists,
            message: format!("a system profile with id {id} already exists"),
        }
    }

    pub fn profile_in_use(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileInUse,
            message: format!("system profile {id} is referenced by at least one TV"),
        }
    }

    pub fn unknown_rpc(verb: &str) -> Self {
        Self {
            code: ErrorCode::UnknownRpc,
            message: format!("no such rpc: {verb}"),
        }
    }

    pub fn discovery_not_found(session: Uuid) -> Self {
        Self {
            code: ErrorCode::DiscoveryNotFound,
            message: format!("no discovery session {session}"),
        }
    }

    pub fn daemon_shutting_down() -> Self {
        Self {
            code: ErrorCode::DaemonShuttingDown,
            message: "world reactor is gone".into(),
        }
    }

    pub fn no_store() -> Self {
        Self {
            code: ErrorCode::NoStore,
            message: "this service runs without a state file".into(),
        }
    }

    /// A state file couldn't be read or parsed; `report` says where
    /// and why, for a person to read.
    pub fn store_unreadable(report: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::StoreUnreadable,
            message: report.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_as_their_strings() {
        for code in ErrorCode::ALL {
            let json = serde_json::to_string(code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), *code);
        }
    }

    #[test]
    fn an_unknown_code_does_not_decode() {
        let error = r#"{"code":"teleport-failed","message":"no"}"#;
        assert!(serde_json::from_str::<WireError>(error).is_err());
    }

    #[test]
    fn invalid_field_splits_on_the_first_separator() {
        fn parts(error: &WireError) -> Option<(&str, &str)> {
            invalid_field_parts(error.code, &error.message)
        }
        let error = WireError::invalid_field("ip", "not an IP address: ::1::bad");
        assert_eq!(parts(&error), Some(("ip", "not an IP address: ::1::bad")));
        assert_eq!(parts(&WireError::bad_request("ip: no")), None);
    }
}
