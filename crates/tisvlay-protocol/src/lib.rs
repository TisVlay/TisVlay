//! Shared wire vocabulary for the TisVlay zenoh stack.
//!
//! Everything both sides of the wire must agree on lives here: the key space,
//! the payload structs, and the session bootstrap (transport endpoint +
//! scouting discipline). Key-space reference: the "Key space" section of
//! `docs/architecture.md`.

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
