//! The ECS world: state machines for TVs, profiles, and pairing.
//!
//! Architecture: all async I/O lives in
//! tokio tasks; the bevy world mutates only at sync points inside
//! [`Reactor::step`]. Inbound facts arrive as [`InEvent`] messages, systems
//! react, and outbound work leaves as effects — wire effects
//! ([`WireEffect`], destined for zenoh in M2) through the effects channel,
//! connection dials as spawned backend tasks.

mod backend;
mod components;
mod events;
mod reactor;
mod systems;

pub use events::{
    BackendCmd, BackendEvent, Command, InEvent, PairingUpdate, ProfileRecord, ReplyTo,
    RouteRequest, StoreSnapshot, SystemEvent, TvRecord, WireEffect, WireError,
};
pub use reactor::{Reactor, WorldClient};
pub use systems::{CONNECT_BACKOFF, CONNECT_MAX_ATTEMPTS};
