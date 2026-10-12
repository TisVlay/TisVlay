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
    /// A per-TV rpc names a TV the daemon doesn't hold.
    TvNotFound = "tv-not-found",
    /// Enrolling, pairing, or a store reload would add a TV under an id
    /// the daemon already holds.
    TvExists = "tv-exists",
    /// The TV has no live connection to carry the command.
    NotConnected = "not-connected",
    /// The request names a vendor with no registered backend.
    UnknownVendor = "unknown-vendor",
    /// A pairing session for the TV is already running.
    PairingExists = "pairing-exists",
    /// A cancel names a TV with no pairing session running.
    PairingNotFound = "pairing-not-found",
    /// A power-on for a disconnected TV whose transport can't wake it.
    WakeNotSupported = "wake-not-supported",
    /// The request body doesn't decode as the rpc's request type, or a
    /// value in it is refused (an immutable config key, a renamed
    /// default profile, a store file listing an id twice).
    BadRequest = "bad-request",
    /// A vendor field failed validation (see [`WireError::invalid_field`]).
    InvalidField = "invalid-field",
    /// A delete, by rpc or by a store reload, targets the reserved
    /// default profile.
    ProfileReserved = "profile-reserved",
    /// The TV refused or failed the command.
    CommandFailed = "command-failed",
    /// The request names a profile the service doesn't hold.
    ProfileNotFound = "profile-not-found",
    /// A create, duplicate, or store reload would add a profile under an
    /// id already in use.
    ProfileExists = "profile-exists",
    /// A delete, by rpc or by a store reload, targets a profile some TV
    /// still references.
    ProfileInUse = "profile-in-use",
    /// The query's key names no rpc of the family it reached.
    UnknownRpc = "unknown-rpc",
    /// A stop names a discovery session that isn't running.
    DiscoveryNotFound = "discovery-not-found",
    /// The daemon's world stopped before it could answer.
    DaemonShuttingDown = "daemon-shutting-down",
    /// The service runs without a state file, so there's nothing to
    /// reload.
    NoStore = "no-store",
    /// The state file couldn't be read or parsed; a reload applies
    /// nothing.
    StoreUnreadable = "store-unreadable",
    /// The service failed on its side; the request itself may be fine.
    Internal = "internal",
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
    /// Why the service refused; clients branch on this.
    pub code: ErrorCode,
    /// The refusal for a person to read. Only an
    /// [`ErrorCode::InvalidField`] message has a shape to parse.
    pub message: String,
}

impl WireError {
    /// No TV has id `id`.
    pub fn tv_not_found(id: Uuid) -> Self {
        Self {
            code: ErrorCode::TvNotFound,
            message: format!("no TV with id {id}"),
        }
    }

    /// A TV with id `id` is already enrolled.
    pub fn tv_exists(id: Uuid) -> Self {
        Self {
            code: ErrorCode::TvExists,
            message: format!("a TV with id {id} already exists"),
        }
    }

    /// TV `id` has no live connection to send the command over.
    pub fn not_connected(id: Uuid) -> Self {
        Self {
            code: ErrorCode::NotConnected,
            message: format!("TV {id} has no live connection"),
        }
    }

    /// No backend is registered under `vendor`.
    pub fn unknown_vendor(vendor: &str) -> Self {
        Self {
            code: ErrorCode::UnknownVendor,
            message: format!("no backend registered for vendor '{vendor}'"),
        }
    }

    /// A pairing session for TV `id` is already running.
    pub fn pairing_exists(id: Uuid) -> Self {
        Self {
            code: ErrorCode::PairingExists,
            message: format!("a pairing session for {id} is already running"),
        }
    }

    /// No pairing session is running for TV `id`.
    pub fn pairing_not_found(id: Uuid) -> Self {
        Self {
            code: ErrorCode::PairingNotFound,
            message: format!("no pairing session for {id}"),
        }
    }

    /// TV `id` is disconnected and its transport can't wake it.
    pub fn wake_not_supported(id: Uuid) -> Self {
        Self {
            code: ErrorCode::WakeNotSupported,
            message: format!("TV {id} is disconnected and its transport cannot wake it"),
        }
    }

    /// The request is malformed or names a refused value; `message`
    /// says which, for a person to read.
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

    /// No profile has id `id`.
    pub fn profile_not_found(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileNotFound,
            message: format!("no system profile with id {id}"),
        }
    }

    /// A profile with id `id` already exists.
    pub fn profile_exists(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileExists,
            message: format!("a system profile with id {id} already exists"),
        }
    }

    /// At least one TV references profile `id`.
    pub fn profile_in_use(id: Uuid) -> Self {
        Self {
            code: ErrorCode::ProfileInUse,
            message: format!("system profile {id} is referenced by at least one TV"),
        }
    }

    /// No rpc of the family answers to `verb`: a query's verb, or its
    /// whole key when no verb splits out of it.
    pub fn unknown_rpc(verb: &str) -> Self {
        Self {
            code: ErrorCode::UnknownRpc,
            message: format!("no such rpc: {verb}"),
        }
    }

    /// No discovery session `session` is running.
    pub fn discovery_not_found(session: Uuid) -> Self {
        Self {
            code: ErrorCode::DiscoveryNotFound,
            message: format!("no discovery session {session}"),
        }
    }

    /// The daemon's world has stopped, so a filed rpc will never be
    /// answered.
    pub fn daemon_shutting_down() -> Self {
        Self {
            code: ErrorCode::DaemonShuttingDown,
            message: "world reactor is gone".into(),
        }
    }

    /// A reload asked of a service running without a state file.
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

    /// The service failed on its side (a task it needs is gone, a reply
    /// it built is malformed); the request itself may be fine.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
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
