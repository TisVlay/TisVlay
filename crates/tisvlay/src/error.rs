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
    /// The service refused the request.
    #[error("{message} ({code})")]
    Rpc { code: ErrorCode, message: String },
    /// The service isn't running, or isn't reachable yet.
    #[error("the {service} is unavailable")]
    Unavailable { service: Service },
    /// No answer (timed out, or the service dropped the call). For a
    /// write, the outcome is unknown.
    #[error("the {service} didn't answer")]
    Timeout { service: Service },
    /// The local link failed: opening the session, a subscription, or a
    /// stream that ended early.
    #[error("{what}")]
    Transport {
        what: &'static str,
        #[source]
        source: Option<BoxError>,
    },
    /// A reply didn't decode as its rpc's response type.
    #[error("malformed reply from the {service}")]
    Protocol {
        service: Service,
        #[source]
        source: BoxError,
    },
}

/// Which service a call went to.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Daemon,
    Session,
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
