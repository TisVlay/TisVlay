//! World-side components and resources. Wire-visible state components live
//! in `tvnext_wire::payload` (component = key = payload); everything here is
//! internal structure.

use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::events::{BackendCmd, SystemEvent, WireEffect};

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
/// the attempt budget is spent. Inserted by AddTv / Reconnect / a wake-path
/// PowerOn — never by a disconnect (a TV that goes down stays down until
/// asked).
#[derive(Component, Debug)]
pub(crate) struct PendingConnect {
    pub attempts: u8,
    /// Dial via `wake_and_connect` (WoL/CEC wake) instead of plain connect.
    pub wake: bool,
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

/// A running pairing session. Lives on its own entity (the TV entity does
/// not exist until pairing completes); despawned on any terminal outcome.
#[derive(Component, Debug)]
pub(crate) struct PairingRef {
    pub name: String,
    pub vendor: String,
    /// The pairing-form params — fed to `build_config_from_pairing` on
    /// success.
    pub params: toml::Value,
    pub managed: bool,
    /// Cancel channel, attached once the pairing task starts. `None` for
    /// the brief window before the task reports in.
    pub cancel_tx: Option<mpsc::Sender<tisvlay_core::PairingCommand>>,
    /// A cancel arrived before `cancel_tx` did — forward it on attach.
    pub cancel_requested: bool,
}

/// Uuid → pairing-session entity index.
#[derive(Resource, Debug, Default)]
pub(crate) struct PairingIndex(pub HashMap<Uuid, Entity>);

/// What a system event does to a TV — the action vocabulary profiles map
/// events onto. The stock profile constructs only a subset; the rest are
/// selected through profile configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ProfileAction {
    ScreenOff,
    ScreenOn,
    PowerOff,
    PowerOn,
}

/// Event → action mapping dispatched to every managed, connected TV. A
/// plain data resource: the stock mapping below is the default profile,
/// replaced wholesale once per-TV profile entities land (M2 CRUD).
#[derive(Resource, Debug)]
pub(crate) struct SystemProfile(pub HashMap<SystemEvent, ProfileAction>);

impl SystemProfile {
    /// Stock mapping — parity with today's default system profile
    /// (screen-blank on lock/sleep/DPMS-off, wake the panel on the way
    /// back, full power-off on shutdown).
    pub(crate) fn stock() -> Self {
        Self(HashMap::from([
            (SystemEvent::Lock, ProfileAction::ScreenOff),
            (SystemEvent::Unlock, ProfileAction::ScreenOn),
            (SystemEvent::Sleep, ProfileAction::ScreenOff),
            (SystemEvent::Resume, ProfileAction::ScreenOn),
            (SystemEvent::Shutdown, ProfileAction::PowerOff),
            (SystemEvent::DpmsOff, ProfileAction::ScreenOff),
            (SystemEvent::DpmsOn, ProfileAction::ScreenOn),
        ]))
    }
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
        wake: bool,
    },
    /// Start a vendor pairing handshake (reactor calls the connector).
    StartPairing {
        tv: Uuid,
        vendor: String,
        params: toml::Value,
    },
    /// Build the persistable vendor config from pairing credentials.
    CompletePairing {
        tv: Uuid,
        vendor: String,
        params: toml::Value,
        credentials: Box<tisvlay_core::Credentials>,
    },
}
