//! The SDK's error: named for what a caller can act on. Dependency
//! errors ride as boxed sources and never appear in a signature.

use std::fmt;

use tisvlay_protocol::caller::RpcError;
use tisvlay_protocol::error::{ErrorCode, invalid_field_parts};

/// A boxed cause, kept for `source()` chains and logs.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Why an SDK call failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The service refused the request. Nothing was applied; branch on
    /// `code` (see also [`Error::invalid_field`]).
    #[error("{message} ({code})")]
    Rpc {
        /// Why the service refused, for a caller to branch on.
        code: ErrorCode,
        /// The service's explanation, in English, for a person to read.
        message: String,
    },
    /// The service isn't running, or isn't reachable yet: no service
    /// took the call before the deadline, so a write wasn't applied.
    /// Retry once the client's `is_available` turns true. A socket this
    /// process may not open looks the same.
    #[error("the {service} is unavailable")]
    Unavailable {
        /// The service the call went to.
        service: Service,
    },
    /// No answer (timed out, or the service dropped the call). For a
    /// write, the outcome is unknown.
    #[error("the {service} didn't answer")]
    Timeout {
        /// The service the call went to.
        service: Service,
    },
    /// The local link failed: opening the session, a subscription, or a
    /// stream that ended early.
    #[error("{what}")]
    Transport {
        /// What the SDK was doing when the link failed.
        what: &'static str,
        /// The underlying failure, when there is one.
        #[source]
        source: Option<BoxError>,
    },
    /// The service runs a release this client can't speak to (a
    /// different `major.minor`). Restart the service or upgrade so the two
    /// match.
    #[error("the {service} runs release {theirs}, which this client ({ours}) can't talk to")]
    VersionMismatch {
        /// The mismatched service.
        service: Service,
        /// The protocol release this SDK speaks.
        ours: String,
        /// The service's release.
        theirs: String,
    },
    /// A reply didn't decode as its rpc's response type.
    #[error("malformed reply from the {service}")]
    Protocol {
        /// The service that sent the reply.
        service: Service,
        /// The decode failure, naming the rpc.
        #[source]
        source: BoxError,
    },
}

/// Which service a call went to.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// The system daemon, which drives the TVs and owns pairing,
    /// discovery and the system profiles ([`DaemonClient`]).
    ///
    /// [`DaemonClient`]: crate::DaemonClient
    Daemon,
    /// The per-user session service, which owns the user profiles and
    /// the stream state ([`SessionClient`]).
    ///
    /// [`SessionClient`]: crate::SessionClient
    Session,
    /// The tray's per-user notification settings service
    /// ([`TrayClient`]).
    ///
    /// [`TrayClient`]: crate::TrayClient
    Tray,
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Daemon => "daemon",
            Self::Session => "session service",
            Self::Tray => "tray",
        })
    }
}

/// The SDK's result type.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The service's refusal code, when the service refused.
    pub fn rpc_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Rpc { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The `(field, reason)` an `invalid-field` refusal names.
    pub fn invalid_field(&self) -> Option<(&str, &str)> {
        match self {
            Self::Rpc { code, message } => invalid_field_parts(*code, message),
            _ => None,
        }
    }

    /// A local-link failure with its cause.
    pub(crate) fn transport(what: &'static str, source: impl Into<BoxError>) -> Self {
        Self::Transport {
            what,
            source: Some(source.into()),
        }
    }

    /// Map a call's outcome to the SDK's error, naming the service. A
    /// transport or decode failure keeps the call's error (its key, and
    /// zenoh's or serde's cause) as the source.
    pub(crate) fn from_rpc(service: Service, error: RpcError) -> Self {
        match error {
            RpcError::Service(refusal) => Self::Rpc {
                code: refusal.code,
                message: refusal.message,
            },
            RpcError::Unavailable { .. } => Self::Unavailable { service },
            RpcError::Timeout { .. } | RpcError::NoReply { .. } => Self::Timeout { service },
            error @ RpcError::Transport { .. } => Self::Transport {
                what: "rpc transport failure",
                source: Some(Box::new(error)),
            },
            error @ RpcError::Undecodable { .. } => Self::Protocol {
                service,
                source: Box::new(error),
            },
            // A failure the protocol added later: keep it as the source.
            error => Self::Transport {
                what: "rpc failure",
                source: Some(Box::new(error)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_protocol::error::WireError;

    #[test]
    fn transport_keeps_its_cause_in_source() {
        let cause = std::io::Error::new(std::io::ErrorKind::NotFound, "no such socket");
        let error = Error::transport("connect to the daemon socket", cause);
        assert_eq!(error.to_string(), "connect to the daemon socket");
        let source = std::error::Error::source(&error).expect("transport carries its cause");
        assert_eq!(source.to_string(), "no such socket");
    }

    #[test]
    fn call_outcomes_map_onto_the_service() {
        let key = || "tisvlay/site/manager/rpc/pair".to_string();
        let refusal = Error::from_rpc(
            Service::Daemon,
            RpcError::Service(WireError::invalid_field("ip", "required")),
        );
        assert_eq!(refusal.rpc_code(), Some(ErrorCode::InvalidField));
        assert_eq!(refusal.invalid_field(), Some(("ip", "required")));
        assert!(matches!(
            Error::from_rpc(Service::Session, RpcError::NoReply { key: key() }),
            Error::Timeout {
                service: Service::Session
            }
        ));
        let malformed = Error::from_rpc(
            Service::Tray,
            RpcError::Undecodable {
                key: key(),
                source: serde_json::from_str::<u8>("\"x\"").unwrap_err(),
            },
        );
        assert!(matches!(
            malformed,
            Error::Protocol {
                service: Service::Tray,
                ..
            }
        ));
        // The chain names the rpc and serde's reason, for the log line
        // a version-skewed client produces.
        let mut chain = malformed.to_string();
        let mut source = std::error::Error::source(&malformed);
        while let Some(cause) = source {
            chain.push_str(": ");
            chain.push_str(&cause.to_string());
            source = cause.source();
        }
        assert!(chain.contains("manager/rpc/pair"), "{chain}");
        assert!(chain.contains("invalid type"), "{chain}");
        assert_eq!(
            Error::from_rpc(Service::Daemon, RpcError::Unavailable { key: key() }).rpc_code(),
            None
        );
    }
}
