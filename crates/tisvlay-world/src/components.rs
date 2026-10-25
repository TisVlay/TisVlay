//! World-side components and resources. Wire-visible state components live
//! in `tisvlay_protocol::payload` (component = key = payload); everything here is
//! internal structure.

use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::*;
use bevy_ecs::world::DeferredWorld;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};
use tisvlay_core::{PowerClass, VendorConfig};
use tisvlay_protocol::payload::StatePayload;
use tisvlay_protocol::{keys, payload};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::events::{BackendCmd, StoreSnapshot, SystemEvent, TransportInfo, WireEffect};

/// Stable identity of a TV entity — the uuid chunk of its keys.
///
/// Immutable, and its hooks keep [`TvIndex`] in step with the live TV
/// entities: inserting it indexes the TV, and removing it (despawn
/// included) unindexes it, deletes every key the TV published, and
/// releases its route monitoring. No spawn or despawn path can forget
/// either. Hooks run when `Commands` flush, so within one system the
/// index lags that system's own spawns and despawns.
#[derive(Component, Debug)]
#[component(immutable, on_insert = index_tv, on_discard = unindex_tv, on_remove = retire_tv)]
pub(crate) struct TvId(pub Uuid);

fn index_tv(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<TvId>(ctx.entity)
        .expect("on_insert runs with TvId in place")
        .0;
    world.resource_mut::<TvIndex>().0.insert(id, ctx.entity);
}

fn unindex_tv(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<TvId>(ctx.entity)
        .expect("on_discard runs with TvId in place")
        .0;
    world.resource_mut::<TvIndex>().0.remove(id, ctx.entity);
}

fn retire_tv(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<TvId>(ctx.entity)
        .expect("on_remove runs with TvId in place")
        .0;
    // A live twin under the same id (a collision `index_tv` logged) keeps
    // its keys.
    if world.resource::<TvIndex>().get(&id).is_some() {
        return;
    }
    world.resource_mut::<PowerEdges>().0.remove(&id);
    let site = world.resource::<Site>().0.clone();
    let mut outbox = world.resource_mut::<Outbox>();
    for path in payload::TV_PATHS
        .iter()
        .chain(payload::TV_EXTRA_PATHS.iter())
    {
        outbox.0.push(Effect::Wire(WireEffect::Delete {
            key: keys::tv_path(&site, &id, path),
        }));
    }
    outbox.0.push(Effect::ForgetMonitors { tv: id });
}

/// A uuid → entity map kept by component hooks. Readable anywhere; only
/// the hooks in this module write it.
#[derive(Debug, Default)]
pub(crate) struct UuidMap(HashMap<Uuid, Entity>);

impl UuidMap {
    pub(crate) fn get(&self, id: &Uuid) -> Option<Entity> {
        self.0.get(id).copied()
    }

    pub(crate) fn contains(&self, id: &Uuid) -> bool {
        self.0.contains_key(id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (Uuid, Entity)> + '_ {
        self.0.iter().map(|(&id, &entity)| (id, entity))
    }

    #[cfg(debug_assertions)]
    pub(crate) fn snapshot(&self) -> HashMap<Uuid, Entity> {
        self.0.clone()
    }

    fn insert(&mut self, id: Uuid, entity: Entity) {
        if let Some(previous) = self.0.insert(id, entity)
            && previous != entity
        {
            tracing::error!(%id, ?previous, ?entity, "two entities claim one id; the newer wins the index");
        }
    }

    /// Unindex `id` if it still names `entity`: a collision's loser
    /// leaving must not unindex the winner.
    fn remove(&mut self, id: Uuid, entity: Entity) {
        if self.0.get(&id) == Some(&entity) {
            self.0.remove(&id);
        }
    }
}

/// A component that keys its entity in a [`UuidMap`] index.
#[cfg(debug_assertions)]
pub(crate) trait Keyed: Component {
    fn key(&self) -> Uuid;
}

#[cfg(debug_assertions)]
impl Keyed for TvId {
    fn key(&self) -> Uuid {
        self.0
    }
}

#[cfg(debug_assertions)]
impl Keyed for ProfileId {
    fn key(&self) -> Uuid {
        self.0
    }
}

#[cfg(debug_assertions)]
impl Keyed for PairingFor {
    fn key(&self) -> Uuid {
        self.0
    }
}

/// Which backend dials this TV, and with what vendor config.
#[derive(Component, Debug)]
pub(crate) struct VendorRef {
    pub vendor: String,
    pub config: VendorConfig,
}

/// Marker: this TV should be dialed and driven by profiles.
#[derive(Component, Debug)]
pub(crate) struct Managed;

/// Marker: a dial task is in flight right now.
#[derive(Component, Debug)]
pub(crate) struct Reconnecting;

/// The most recently spawned dial for this TV, numbered from [`DialSeq`].
/// Every backend event carries the dial its task was spawned for; events
/// from any other (superseded) dial are dropped, so a dial a re-pair
/// overtook can't settle the verification or touch the live link.
#[derive(Component, Debug)]
pub(crate) struct LatestDial(pub u64);

/// Monotonic dial numbering for [`LatestDial`].
#[derive(Resource, Debug, Default)]
pub(crate) struct DialSeq(pub u64);

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

/// A dial request deferred on a known absence of what the connection
/// depends on: a usable route, or the TV's local device node —
/// `reconcile_connections` skips the entity until the netlink monitor or
/// the device watcher reports it back. Marks the deferral episode, so it
/// logs once; a wake's waiting phase goes out when its [`HeldWake`]
/// starts, which also covers a wake upgrading an episode already under
/// way. The dial gate itself is [`dependency_absent`], so the facts and
/// the gate cannot drift.
#[derive(Component, Debug)]
pub(crate) struct AwaitingDependency;

/// Connection parked because what it depends on went away (carrier down,
/// route lost, device node removed); that dependency's return redials.
#[derive(Component, Debug)]
pub(crate) struct Parked;

/// Whether the TV's local device node is usable, as the daemon's device
/// watcher last reported. Present only on TVs whose connector names a
/// local device, and only once a report has arrived.
#[derive(Component, Debug, PartialEq, Eq)]
pub(crate) struct DeviceAvailable(pub bool);

/// A wake dial has been held on a missing dependency since this instant.
/// Held past `WAKE_HOLD_LIMIT` it expires into a plain reconnect, so a
/// route or adapter that returns hours later never powers the TV on.
/// Cleared by any new connect request (`redial`) and when the dial goes
/// out.
#[derive(Component, Debug)]
pub(crate) struct HeldWake(pub Instant);

/// The TV's device came back while a dial was in flight: if that dial
/// fails, retry at once with a fresh budget instead of waiting out a grown
/// backoff.
#[derive(Component, Debug)]
pub(crate) struct RetryPromptly;

/// A device watcher is attached: a local-device TV's dial then waits for
/// the watcher's first report instead of racing it.
#[derive(Resource, Debug, Default)]
pub(crate) struct DeviceWatch(pub bool);

/// The dial gate's shared absence test: the route is known gone, or the
/// device node is known gone. Component absence is never absence: a TV
/// nobody has reported on yet dials.
pub(crate) fn dependency_absent(
    monitored: Option<&MonitoredInterface>,
    device: Option<&DeviceAvailable>,
) -> bool {
    matches!(monitored, Some(MonitoredInterface(None)))
        || matches!(device, Some(DeviceAvailable(false)))
}

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
    /// The pairing attempt whose terminal this dial publishes.
    pub attempt: Uuid,
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
    /// The attempt id stamped on every progress sample of this session.
    pub attempt: Uuid,
    pub vendor: String,
    /// The pairing-form params — fed to `build_config_from_pairing` on
    /// success.
    pub params: VendorConfig,
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

/// The TV a pairing session is for. Immutable, and its hooks keep
/// [`PairingIndex`] in step with the live sessions (see [`TvId`]).
#[derive(Component, Debug)]
#[component(immutable, on_insert = index_pairing, on_discard = unindex_pairing)]
pub(crate) struct PairingFor(pub Uuid);

fn index_pairing(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<PairingFor>(ctx.entity)
        .expect("on_insert runs with PairingFor in place")
        .0;
    world
        .resource_mut::<PairingIndex>()
        .0
        .insert(id, ctx.entity);
}

fn unindex_pairing(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<PairingFor>(ctx.entity)
        .expect("on_discard runs with PairingFor in place")
        .0;
    world
        .resource_mut::<PairingIndex>()
        .0
        .remove(id, ctx.entity);
}

/// TV uuid → pairing-session entity, kept by [`PairingFor`]'s hooks.
#[derive(Resource, Debug, Default)]
pub(crate) struct PairingIndex(UuidMap);

impl Deref for PairingIndex {
    type Target = UuidMap;
    fn deref(&self) -> &UuidMap {
        &self.0
    }
}

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

/// Event → `(action, restore_input, while_streaming)` mapping applied when
/// a TV has no assigned profile or its profile leaves the event's field
/// unset — the inheritance base the `profile/rpc/defaults` projection
/// serves. The trailing bool is the resolved stream gate: `true`
/// fires while the TV is armed, `false` suppresses.
#[derive(Resource, Debug)]
pub(crate) struct SystemProfile(pub HashMap<SystemEvent, (ProfileAction, bool, bool)>);

impl SystemProfile {
    /// Stock mapping — parity with the root stack's hard-coded resolved
    /// defaults: full power-off on sleep/shutdown, wake-on-LAN on resume,
    /// screen-blank mirroring for DPMS and the login greeter's idle timer.
    /// The session-start wakes restore the primary input (mirrors
    /// `ResolvedSystemProfile::default`).
    ///
    /// The `while_streaming` floor splits by event direction, mirroring
    /// [`tisvlay_core::ResolvedSystemProfile::default`]: off-going events
    /// fire while armed (a blank is always safe to let through, and the
    /// machine going away turns the TV off for the empty room), so sleep,
    /// shutdown, and the DPMS-off/greeter-idle blanks are ungated; the
    /// wake-ish events (startup, resume, DPMS-on, greeter-resume) stay
    /// gated. Greeter events run before any session, so their gate never
    /// bites either way (the armed set is empty at the login screen).
    pub(crate) fn stock() -> Self {
        Self(HashMap::from([
            // WoL at boot; a reboot deliberately has no stock action
            // (the TV stays on across a quick restart).
            (SystemEvent::Startup, (ProfileAction::PowerOn, true, false)),
            (SystemEvent::Sleep, (ProfileAction::PowerOff, false, true)),
            (SystemEvent::Resume, (ProfileAction::PowerOn, true, false)),
            (
                SystemEvent::Shutdown,
                (ProfileAction::PowerOff, false, true),
            ),
            (
                SystemEvent::DpmsOff,
                (ProfileAction::ScreenOff, false, true),
            ),
            (SystemEvent::DpmsOn, (ProfileAction::ScreenOn, false, false)),
            (
                SystemEvent::GreeterIdle,
                (ProfileAction::ScreenOff, false, true),
            ),
            (
                SystemEvent::GreeterResume,
                (ProfileAction::ScreenOn, false, false),
            ),
        ]))
    }
}

/// The set of TVs a stream currently arms — mirrored from the
/// session service (which owns arming policy) via
/// [`InEvent::Stream`](crate::events::InEvent::Stream).
/// The dispatch gate consults membership to suppress events whose action
/// carries a `while_streaming = false` gate; an empty set (no session
/// service, or no active stream) gates nothing.
#[derive(Resource, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamArmed(pub HashSet<Uuid>);

/// Per-TV power-edge tracking: what decides that a TV powered off by
/// itself. A resource rather than components, so the reports of one batch
/// see each other's updates at once (a component inserted through
/// `Commands` lands only after the system).
#[derive(Resource, Debug, Default)]
pub(crate) struct PowerEdges(pub HashMap<Uuid, PowerEdge>);

/// One TV's power-edge state across its connections.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PowerEdge {
    /// Whether this connection's first state report arrived. Until it
    /// does, a report only sets the baseline: the subscription opens with
    /// the current state (LG's replay, CEC's seed reading or its `Active`
    /// fallback), which is no transition.
    seeded: bool,
    /// The last on or off reading; transient readings don't move it.
    settled: Option<PowerClass>,
    /// When TisVlay last sent the TV a power-off or a blank: the off edge
    /// that follows is its own.
    expect_off: Option<Instant>,
}

impl PowerEdge {
    /// A connection's first reading (the `Connected` snapshot): the
    /// baseline, and the next report is one too.
    pub(crate) fn connected(&mut self, class: PowerClass) {
        self.seeded = false;
        self.settle(class);
    }

    /// A live report: whether it ends an on-to-off edge TisVlay didn't
    /// cause. An expectation within `limit` of `now` consumes the edge.
    pub(crate) fn report(&mut self, class: PowerClass, now: Instant, limit: Duration) -> bool {
        if !self.seeded {
            self.seeded = true;
            self.settle(class);
            return false;
        }
        let edge = class == PowerClass::Off && self.settled == Some(PowerClass::On);
        self.settle(class);
        if !edge {
            return false;
        }
        !self
            .expect_off
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) < limit)
    }

    /// TisVlay is sending the TV a power-off or a blank. Nothing is
    /// recorded for a TV already off: no edge will follow to consume it,
    /// and a stale expectation would swallow the next real one.
    pub(crate) fn sending_off(&mut self, now: Instant) {
        if self.settled != Some(PowerClass::Off) {
            self.expect_off = Some(now);
        }
    }

    /// Forget the expectation: the PC woke, and the world's clock, a
    /// monotonic `Instant`, didn't move while it slept.
    pub(crate) fn forget_expectation(&mut self) {
        self.expect_off = None;
    }

    fn settle(&mut self, class: PowerClass) {
        if class != PowerClass::Transient {
            self.settled = Some(class);
        }
    }
}

/// The `<site>` chunk used in every key this world publishes.
#[derive(Resource, Debug)]
pub(crate) struct Site(pub String);

/// World time, advanced by `InEvent::Tick`. Systems never read wall-clock
/// directly — tests own time.
#[derive(Resource, Debug)]
pub(crate) struct Clock(pub Instant);

/// TV uuid → entity, for O(1) command routing; kept by [`TvId`]'s hooks.
#[derive(Resource, Debug, Default)]
pub(crate) struct TvIndex(UuidMap);

impl Deref for TvIndex {
    type Target = UuidMap;
    fn deref(&self) -> &UuidMap {
        &self.0
    }
}

/// A system profile's stable identity. Immutable, and its hooks keep
/// [`ProfileIndex`] in step and delete the profile's key on removal (see
/// [`TvId`]).
#[derive(Component, Debug)]
#[component(immutable, on_insert = index_profile, on_discard = unindex_profile, on_remove = retire_profile)]
pub(crate) struct ProfileId(pub Uuid);

fn index_profile(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<ProfileId>(ctx.entity)
        .expect("on_insert runs with ProfileId in place")
        .0;
    world
        .resource_mut::<ProfileIndex>()
        .0
        .insert(id, ctx.entity);
}

fn unindex_profile(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<ProfileId>(ctx.entity)
        .expect("on_discard runs with ProfileId in place")
        .0;
    world
        .resource_mut::<ProfileIndex>()
        .0
        .remove(id, ctx.entity);
}

fn retire_profile(mut world: DeferredWorld, ctx: HookContext) {
    let id = world
        .get::<ProfileId>(ctx.entity)
        .expect("on_remove runs with ProfileId in place")
        .0;
    if world.resource::<ProfileIndex>().get(&id).is_some() {
        return;
    }
    let site = world.resource::<Site>().0.clone();
    world
        .resource_mut::<Outbox>()
        .0
        .push(Effect::Wire(WireEffect::Delete {
            key: keys::profile_system(&site, &id),
        }));
}

/// A system profile's body (name + event→action mapping) — the wire
/// payload at `profile/system/<uuid>` verbatim.
#[derive(Component, Debug)]
pub(crate) struct ProfileBody(pub tisvlay_core::SystemProfileConfig);

/// Profile uuid → entity, kept by [`ProfileId`]'s hooks.
#[derive(Resource, Debug, Default)]
pub(crate) struct ProfileIndex(UuidMap);

impl Deref for ProfileIndex {
    type Target = UuidMap;
    fn deref(&self) -> &UuidMap {
        &self.0
    }
}

/// Vendors the reactor has connectors for — AddTv validation.
#[derive(Resource, Debug)]
pub(crate) struct KnownVendors(pub HashSet<String>);

/// The snapshot last handed to the store (or loaded from it at boot):
/// `snapshot_store` persists only when the world's state differs.
#[derive(Resource, Debug, Default)]
pub(crate) struct LastPersisted(pub Option<StoreSnapshot>);

/// Deferred outbound work, drained by the reactor after each schedule run.
#[derive(Resource, Debug, Default)]
pub(crate) struct Outbox(pub Vec<Effect>);

#[derive(Debug)]
pub(crate) enum Effect {
    /// Forward to the wire (the zenoh bridge).
    Wire(WireEffect),
    /// Stop route- and device-monitoring a removed TV.
    ForgetMonitors { tv: Uuid },
    /// Spawn a dial task for this TV.
    SpawnConnect {
        tv: Uuid,
        /// This dial's number; echoed on every event its task sends.
        dial: u64,
        /// A pairing-verification dial: the connect flow also does the
        /// pair-time reads (the device-reported link MAC).
        pairing: bool,
        vendor: String,
        config: VendorConfig,
        wake: bool,
    },
    /// Start a vendor pairing handshake (reactor calls the connector).
    StartPairing {
        tv: Uuid,
        vendor: String,
        params: VendorConfig,
    },
    /// Build the persistable vendor config from pairing credentials.
    CompletePairing {
        tv: Uuid,
        vendor: String,
        params: VendorConfig,
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
        config: VendorConfig,
    },
    /// Persist the full state snapshot (the store writer debounces and
    /// writes atomically).
    Persist(Box<StoreSnapshot>),
    /// Tell the store writer a reload finished (see `StoreMsg::Reloaded`).
    StoreReloaded(Option<Box<StoreSnapshot>>),
}

/// A TV's state payload as a component. The payloads are the protocol's
/// wire types, which know nothing of this ECS, so each is stored wrapped:
/// queries read `&State<payload::Power>`, and the generic publisher keys
/// off the payload's `StatePayload::PATH`.
#[derive(Component, Clone, Debug, PartialEq)]
pub(crate) struct State<T: StatePayload>(pub T);

impl<T: StatePayload> Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: StatePayload> DerefMut for State<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

#[cfg(test)]
mod power_edge_tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(60);

    /// A connected TV whose subscription opened with an on reading.
    fn live_on(now: Instant) -> PowerEdge {
        let mut edge = PowerEdge::default();
        edge.connected(PowerClass::On);
        assert!(
            !edge.report(PowerClass::On, now, LIMIT),
            "the seed is no edge"
        );
        edge
    }

    #[test]
    fn a_live_power_off_is_an_edge() {
        let now = Instant::now();
        let mut edge = live_on(now);
        assert!(edge.report(PowerClass::Off, now, LIMIT));
        assert!(!edge.report(PowerClass::Off, now, LIMIT), "only once");
    }

    /// The reconnect after a keyboard wake reads standby against the
    /// cached on: a baseline, not the TV powering off.
    #[test]
    fn first_readings_set_the_baseline() {
        let now = Instant::now();
        let mut edge = live_on(now);
        edge.connected(PowerClass::Off);
        assert!(!edge.report(PowerClass::Off, now, LIMIT));

        // CEC's seed (or its `Active` fallback) disagreeing with the
        // snapshot is a baseline too.
        let mut edge = live_on(now);
        edge.connected(PowerClass::On);
        assert!(!edge.report(PowerClass::Off, now, LIMIT));
    }

    #[test]
    fn transient_readings_never_end_an_edge() {
        let now = Instant::now();
        let mut edge = live_on(now);
        assert!(!edge.report(PowerClass::Transient, now, LIMIT));
        assert!(edge.report(PowerClass::Off, now, LIMIT), "on, cooling, off");
        assert!(!edge.report(PowerClass::Transient, now, LIMIT));
        assert!(
            !edge.report(PowerClass::Off, now, LIMIT),
            "off, unknown, off"
        );
    }

    #[test]
    fn tisvlays_own_power_off_is_consumed_until_it_expires() {
        let now = Instant::now();
        let mut edge = live_on(now);
        edge.sending_off(now);
        assert!(!edge.report(PowerClass::Off, now + Duration::from_secs(1), LIMIT));

        let mut edge = live_on(now);
        edge.sending_off(now);
        assert!(edge.report(PowerClass::Off, now + LIMIT, LIMIT), "expired");
    }

    /// A power-off sent to a TV already off records nothing, so the next
    /// real power-off still counts.
    #[test]
    fn nothing_is_expected_of_a_tv_already_off() {
        let now = Instant::now();
        let mut edge = live_on(now);
        assert!(edge.report(PowerClass::Off, now, LIMIT));
        edge.sending_off(now);
        assert!(!edge.report(PowerClass::On, now, LIMIT));
        assert!(edge.report(PowerClass::Off, now, LIMIT));
    }

    #[test]
    fn waking_forgets_the_expectation() {
        let now = Instant::now();
        let mut edge = live_on(now);
        edge.sending_off(now);
        edge.forget_expectation();
        assert!(edge.report(PowerClass::Off, now, LIMIT));
    }
}
