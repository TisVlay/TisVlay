//! World-side components and resources. Wire-visible state components live
//! in `tvnext_wire::payload` (component = key = payload); everything here is
//! internal structure.

use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::events::{BackendCmd, WireEffect};

/// Stable identity of a TV entity — the uuid chunk of its keys.
#[derive(Component, Debug)]
pub(crate) struct TvId(pub Uuid);

/// Which backend dials this TV, and with what vendor config.
#[derive(Component, Debug)]
pub(crate) struct VendorRef {
    pub vendor: String,
    pub config: toml::Value,
}

/// Marker: this TV should be dialed and driven by profiles.
#[derive(Component, Debug)]
pub(crate) struct Managed;

/// Marker: a dial task is in flight right now.
#[derive(Component, Debug)]
pub(crate) struct Reconnecting;

/// A connect request is outstanding: reconcile dials until it succeeds or
/// the attempt budget is spent. Inserted by AddTv / Reconnect — never by a
/// disconnect (a TV that goes down stays down until asked).
#[derive(Component, Debug)]
pub(crate) struct PendingConnect {
    pub attempts: u8,
}

/// Backoff gate: no new dial before this instant.
#[derive(Component, Debug)]
pub(crate) struct RetryAt(pub Instant);

/// Live connection: channel into the tokio task that owns the
/// `TvConnection`. Dropping this (despawn, disconnect) closes the channel
/// and the task shuts the connection down — RAII instead of a drain layer.
#[derive(Component, Debug)]
pub(crate) struct ConnectionHandle {
    pub cmd_tx: mpsc::UnboundedSender<BackendCmd>,
}

/// The `<site>` chunk used in every key this world publishes.
#[derive(Resource, Debug)]
pub(crate) struct Site(pub String);

/// World time, advanced by `InEvent::Tick`. Systems never read wall-clock
/// directly — tests own time.
#[derive(Resource, Debug)]
pub(crate) struct Clock(pub Instant);

/// Uuid → entity index for O(1) command routing.
#[derive(Resource, Debug, Default)]
pub(crate) struct TvIndex(pub HashMap<Uuid, Entity>);

/// Vendors the reactor has connectors for — AddTv validation.
#[derive(Resource, Debug)]
pub(crate) struct KnownVendors(pub HashSet<String>);

/// Deferred outbound work, drained by the reactor after each schedule run.
#[derive(Resource, Debug, Default)]
pub(crate) struct Outbox(pub Vec<Effect>);

#[derive(Debug)]
pub(crate) enum Effect {
    /// Forward to the wire (zenoh in M2, effects channel today).
    Wire(WireEffect),
    /// Spawn a dial task for this TV.
    SpawnConnect {
        tv: Uuid,
        vendor: String,
        config: toml::Value,
    },
}
