//! World-side components and resources. Wire-visible state components live
//! in `tisvlay_wire::payload` (component = key = payload); everything here is
//! internal structure.

use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::events::{
    BackendCmd, ReplyTo, StoreSnapshot, SystemEvent, TransportInfo, TvRecord, WireEffect,
};

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

/// A dial request deferred on a known absence of a usable route —
/// `reconcile_connections` skips the entity until the netlink monitor
/// delivers one. Publish gate only: its insertion edge emits the wake
/// path's `awaiting_network` phase once per deferral episode; the dial
/// gate itself is the `MonitoredInterface` check, so the two facts
/// cannot drift.
#[derive(Component, Debug)]
pub(crate) struct AwaitingRoute;

/// Connection parked by a carrier-down or route-lost event; the next
/// carrier-up or route-restored on the monitored interface redials.
#[derive(Component, Debug)]
pub(crate) struct CarrierParked;

/// The NIC carrying traffic to this TV, resolved from the kernel routing
/// table by the daemon's netlink monitor (`ip route get <address>`).
/// `None` = no usable route right now. Present only once a resolution
/// has arrived, so it exists solely on network-dependent TVs with an
/// IP-parseable address — carrier events match against it, and TVs
/// without it ignore them.
#[derive(Component, Debug, PartialEq, Eq)]
pub(crate) struct MonitoredInterface(pub Option<String>);

/// The DRM connector this TV mirrors (desktop-probe match) — scopes DPMS
/// events to the right TV.
#[derive(Component, Debug)]
pub(crate) struct MirrorsConnector(pub String);

/// Live connection: channel into the tokio task that owns the
/// `TvConnection`. Dropping this (despawn, disconnect) closes the channel
/// and the task shuts the connection down — RAII instead of a drain layer.
#[derive(Component, Debug)]
pub(crate) struct ConnectionHandle {
    pub cmd_tx: mpsc::UnboundedSender<BackendCmd>,
}

/// Transport characteristics of this TV's backend. Config-derived
/// statics, evaluated by the reactor at enrollment and on every dial
/// spawn — present before any connection exists, so dispatch decisions
/// (wake path, screen emulation, standby reachability) don't depend on
/// a dial having succeeded.
#[derive(Component, Debug)]
pub(crate) struct TransportTraits(pub TransportInfo);

/// A freshly paired TV whose pairing terminal is gated on its first
/// dial outcome: `complete` publishes only once the TV actually
/// connects, and a failed first dial fails the pairing. `rollback`
/// distinguishes fresh enrollment (failure removes the TV — nothing
/// persists) from re-pairing (failure keeps the existing TV).
#[derive(Component, Debug)]
pub(crate) struct PairVerifying {
    pub rollback: bool,
}

/// Per-backend verify-retry tuning, delivered with the connection.
#[derive(Component, Debug)]
pub(crate) struct DispatchRetry(pub tisvlay_core::RetryConfig);

/// A screen action awaiting verification: after `verify_at`, the cached
/// power state must agree or the action retries — and `screen_on`
/// escalates to the wake path once attempts are spent (the panel saying
/// "ok" while staying dark is real LG firmware behavior). This is the
/// one screen-intent latch: it survives a wake-driven teardown (the
/// dial defers verification; the reconnect re-asserts and restarts the
/// clock) so elevation and escalation never need a second carrier.
#[derive(Component, Debug)]
pub(crate) struct DispatchVerify {
    pub target_on: bool,
    pub attempts: u8,
    pub verify_at: Instant,
    /// The wake escalation already fired for this dispatch. Carried
    /// across the reconnect so a TV that keeps ACKing dark gets exactly
    /// one wake dial per dispatch instead of ping-ponging forever.
    pub escalated: bool,
}

/// A running pairing session, keyed by the TV's uuid. For a fresh pairing
/// the TV entity doesn't exist yet and `enroll` carries its birth
/// metadata; for a re-pair `enroll` is `None` and completion refreshes the
/// existing TV's credentials. Despawned on any terminal outcome.
#[derive(Component, Debug)]
pub(crate) struct PairingRef {
    pub vendor: String,
    /// The pairing-form params — fed to `build_config_from_pairing` on
    /// success.
    pub params: toml::Value,
    /// `Some` = fresh enrollment (spawn on success), `None` = re-pair.
    pub enroll: Option<PairingEnroll>,
    /// Cancel channel, attached once the pairing task starts. `None` for
    /// the brief window before the task reports in.
    pub cancel_tx: Option<mpsc::Sender<tisvlay_core::PairingCommand>>,
    /// A cancel arrived before `cancel_tx` did — forward it on attach.
    pub cancel_requested: bool,
}

#[derive(Debug)]
pub(crate) struct PairingEnroll {
    pub name: String,
    pub managed: bool,
}

/// Uuid → pairing-session entity index.
#[derive(Resource, Debug, Default)]
pub(crate) struct PairingIndex(pub HashMap<Uuid, Entity>);

/// The input this TV should land on once a connection is live —
/// recorded at dispatch time by restore-flagged on-ish actions,
/// drained by `reconcile_input` with a bounded verify-retry, cleared
/// on first compliance or a spent budget. A later manual input change
/// is authoritative: nothing re-arms this without a new dispatch, and
/// a manual `switch-input` rpc removes it outright.
#[derive(Component, Debug)]
pub(crate) struct DesiredInput {
    pub input: String,
    pub attempts: u8,
    pub next_at: Instant,
}

/// What a system event does to a TV — the action vocabulary profiles map
/// events onto. The stock profile constructs only a subset; the rest are
/// selected through profile configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileAction {
    ScreenOff,
    ScreenOn,
    PowerOff,
    PowerOn,
}

/// Event → `(action, restore_input)` mapping applied when a TV has no
/// assigned profile or its profile leaves the event's field unset —
/// the inheritance base the `profile/rpc/defaults` projection serves.
#[derive(Resource, Debug)]
pub(crate) struct SystemProfile(pub HashMap<SystemEvent, (ProfileAction, bool)>);

impl SystemProfile {
    /// Stock mapping — parity with the root stack's hard-coded resolved
    /// defaults: full power-off on sleep/shutdown, wake-on-LAN on resume,
    /// screen-blank mirroring for DPMS and the login greeter's idle timer.
    /// The session-start wakes restore the primary input (mirrors
    /// `ResolvedSystemProfile::default`).
    pub(crate) fn stock() -> Self {
        Self(HashMap::from([
            // WoL at boot; a reboot deliberately has no stock action
            // (the TV stays on across a quick restart).
            (SystemEvent::Startup, (ProfileAction::PowerOn, true)),
            (SystemEvent::Sleep, (ProfileAction::PowerOff, false)),
            (SystemEvent::Resume, (ProfileAction::PowerOn, true)),
            (SystemEvent::Shutdown, (ProfileAction::PowerOff, false)),
            (SystemEvent::DpmsOff, (ProfileAction::ScreenOff, false)),
            (SystemEvent::DpmsOn, (ProfileAction::ScreenOn, false)),
            (SystemEvent::GreeterIdle, (ProfileAction::ScreenOff, false)),
            (SystemEvent::GreeterResume, (ProfileAction::ScreenOn, false)),
        ]))
    }
}

/// The set of TVs a remote session currently arms — mirrored from the
/// session service (which owns arming policy) via
/// [`InEvent::RemoteSession`](crate::events::InEvent::RemoteSession).
/// The dispatch gate consults membership to apply while-remote
/// overrides; an empty set (no session service, or no active remote
/// session) gates nothing.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteArmed(pub HashSet<Uuid>);

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

/// A system profile's stable identity.
#[derive(Component, Debug)]
pub(crate) struct ProfileId(pub Uuid);

/// A system profile's body (name + event→action mapping) — the wire
/// payload at `profile/system/<uuid>` verbatim.
#[derive(Component, Debug)]
pub(crate) struct ProfileBody(pub tisvlay_core::SystemProfileConfig);

/// Uuid → profile entity index.
#[derive(Resource, Debug, Default)]
pub(crate) struct ProfileIndex(pub HashMap<Uuid, Entity>);

/// Vendors the reactor has connectors for — AddTv validation.
#[derive(Resource, Debug)]
pub(crate) struct KnownVendors(pub HashSet<String>);

/// Config-shaped state changed this frame — the persist system snapshots
/// the whole TV table when set.
#[derive(Resource, Debug, Default)]
pub(crate) struct DirtyStore(pub bool);

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
    /// Re-evaluate a TV's config-derived transport facts (the reactor
    /// owns the connectors). Emitted on vendor-config merges so
    /// `supports_wake`/traits refresh even when no dial follows — a
    /// MAC added while the TV is off must light the wake path up
    /// immediately.
    Facts {
        tv: Uuid,
        vendor: String,
        config: toml::Value,
    },
    /// Persist the full state snapshot (reactor debounces and writes
    /// atomically).
    Persist(Box<StoreSnapshot>),
    /// Redact a TV's config against its vendor schema and reply — the
    /// reactor owns the connectors (and thus the schemas), so the Secret
    /// filtering happens there and credentials never reach the bridge.
    RedactConfig { reply: ReplyTo, record: TvRecord },
}
