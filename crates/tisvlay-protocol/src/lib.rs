//! Shared wire vocabulary for the TisVlay zenoh stack.
//!
//! Everything both sides of the wire must agree on lives here: the key space,
//! the payload structs, and the session bootstrap (transport endpoint +
//! scouting discipline). Key-space reference: the "Key space" section of
//! `docs/architecture.md`.

// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

pub mod bootstrap;
pub mod caller;
pub mod endpoint;
pub mod error;
pub mod keys;
pub mod payload;
/// Request bodies for the RPC queryables — shared by the daemon bridge and
/// clients so the wire shape exists in exactly one place.
pub mod rpc;
pub mod serve;

/// This crate's release: the protocol every service and client built from
/// it speaks. Services report it in `rpc::ServiceInfo::version`; clients
/// compare it with [`compatible`].
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether a peer at release `theirs` speaks the protocol of one at
/// `ours`: the same `major.minor`. Releases change the protocol only in a
/// minor bump, so patch releases stay compatible. A release that doesn't
/// parse is incompatible.
pub fn compatible(ours: &str, theirs: &str) -> bool {
    fn line(release: &str) -> Option<(u64, u64)> {
        let mut parts = release.split(['.', '-', '+']);
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    }
    matches!((line(ours), line(theirs)), (Some(ours), Some(theirs)) if ours == theirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_release_line_is_compatible() {
        assert!(compatible("0.4.0", "0.4.7"));
        assert!(compatible("0.4.2", "0.4.0-rc.1"));
        assert!(!compatible("0.4.0", "0.5.0"));
        assert!(!compatible("0.4.0", "1.4.0"));
        assert!(!compatible("0.4.0", "garbage"));
        assert!(!compatible("0.4.0", ""));
        assert!(compatible(VERSION, VERSION));
    }

    /// The handshake surface is frozen: a `ServiceInfo` with fields a
    /// later release added still decodes.
    #[test]
    fn service_info_decodes_with_fields_it_doesnt_know() {
        let info: rpc::ServiceInfo = serde_json::from_str(
            r#"{"version":"0.9.0","state_path":"/var/lib/tisvlay/daemon.toml","added_later":true}"#,
        )
        .unwrap();
        assert_eq!(info.version, "0.9.0");
    }
}
