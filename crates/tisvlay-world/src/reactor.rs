//! The reactor: single owner of the bevy world. Batches inbox events,
//! applies client commands one at a time, runs the schedule over the
//! batch's facts, and executes drained effects.

use bevy_ecs::error::{BevyError, ErrorContext, FallbackErrorHandler, match_severity};
use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemInput;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;
use tisvlay_core::{TvConnector, VendorConfig};
use tisvlay_protocol::endpoint::Rpc;
use tisvlay_protocol::rpc::TvConfigView;
use tokio::sync::mpsc;

use crate::backend;
use crate::components::{
    Clock, ConnectionHandle, DeviceWatch, DialSeq, Effect, GoingOff, KnownVendors, LastPersisted,
    Outbox, PairingIndex, PowerEdges, ProfileIndex, Site, StreamArmed, SystemProfile, TvIndex,
};
use crate::events::{
    AckReply, AnyReply, BackendCmd, BackendEvent, Command, DeviceRequest, InEvent, PairingUpdate,
    Reply, RouteRequest, SleepRequest, StoreMsg, StoreSnapshot, TvRecord, WireEffect, WireError,
};
use crate::systems::build_schedule;
use crate::systems::commands as handle;

pub struct Reactor {
    world: World,
    schedule: Schedule,
    inbox_rx: mpsc::UnboundedReceiver<InEvent>,
    inbox_tx: mpsc::UnboundedSender<InEvent>,
    connectors: HashMap<String, Arc<dyn TvConnector>>,
    effects_tx: mpsc::UnboundedSender<WireEffect>,
    persist_tx: Option<mpsc::UnboundedSender<StoreMsg>>,
    routes_tx: Option<mpsc::UnboundedSender<RouteRequest>>,
    devices_tx: Option<mpsc::UnboundedSender<DeviceRequest>>,
    /// Sleep requests for the daemon's logind adapter.
    sleep_tx: Option<mpsc::UnboundedSender<SleepRequest>>,
    /// The wake list, for the daemon to write where the wakeup unit reads it.
    wake_list_tx: Option<mpsc::UnboundedSender<Vec<std::path::PathBuf>>>,
    vendor_retry: HashMap<String, tisvlay_core::RetryConfig>,
}

impl fmt::Debug for Reactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reactor").finish_non_exhaustive()
    }
}

/// Cloneable sender into the reactor inbox — the seam where the zenoh
/// bridge, watchers, and tests inject events.
#[derive(Clone, Debug)]
pub struct WorldClient {
    tx: mpsc::UnboundedSender<InEvent>,
}

impl WorldClient {
    /// Send a control-plane command. Returns false if the reactor is gone.
    pub fn command(&self, command: Command) -> bool {
        self.send(InEvent::Command(command))
    }

    /// Advance world time.
    pub fn tick(&self, now: Instant) -> bool {
        self.send(InEvent::Tick { now })
    }

    /// Inject a raw event — backend fact injection for tests and adapters.
    pub fn send(&self, event: InEvent) -> bool {
        self.tx.send(event).is_ok()
    }
}

impl Reactor {
    /// Build a reactor for `site` with the given vendor → connector map.
    /// Returns the wire-effects receiver (the zenoh bridge, or a test
    /// tap) and a client for injecting events.
    pub fn new(
        site: impl Into<String>,
        connectors: Vec<(&str, Arc<dyn TvConnector>)>,
    ) -> (Self, mpsc::UnboundedReceiver<WireEffect>, WorldClient) {
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let (effects_tx, effects_rx) = mpsc::unbounded_channel();
        let connectors: HashMap<String, Arc<dyn TvConnector>> = connectors
            .into_iter()
            .map(|(vendor, connector)| (vendor.to_string(), connector))
            .collect();

        let mut world = World::new();
        world.insert_resource(Site(site.into()));
        world.insert_resource(Clock(Instant::now()));
        world.insert_resource(KnownVendors(
            connectors.keys().cloned().collect::<HashSet<_>>(),
        ));
        world.insert_resource(SystemProfile::stock());
        world.init_resource::<StreamArmed>();
        world.init_resource::<PowerEdges>();
        world.init_resource::<GoingOff>();
        world.init_resource::<TvIndex>();
        world.init_resource::<PairingIndex>();
        world.init_resource::<ProfileIndex>();
        world.init_resource::<LastPersisted>();
        world.init_resource::<DialSeq>();
        world.init_resource::<DeviceWatch>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<InEvent>>();
        world.insert_resource(FallbackErrorHandler(log_world_error));

        let client = WorldClient {
            tx: inbox_tx.clone(),
        };
        (
            Self {
                world,
                schedule: build_schedule(),
                inbox_rx,
                inbox_tx,
                connectors,
                effects_tx,
                persist_tx: None,
                routes_tx: None,
                devices_tx: None,
                sleep_tx: None,
                wake_list_tx: None,
                vendor_retry: HashMap::new(),
            },
            effects_rx,
            client,
        )
    }

    /// Attach the store outlet: state snapshots to write, and reload
    /// outcomes, flow here. Without one they are dropped — fine for
    /// ephemeral worlds like tests.
    pub fn set_persist(&mut self, tx: mpsc::UnboundedSender<StoreMsg>) {
        self.persist_tx = Some(tx);
    }

    /// Attach the route-monitor outlet: `Monitor`/`Forget` requests for
    /// network-dependent TVs flow to the daemon's netlink watcher, which
    /// answers with [`InEvent::RouteResolved`]. Without one, no route
    /// monitoring — fine for worlds with no real network (tests).
    pub fn set_route_monitor(&mut self, tx: mpsc::UnboundedSender<RouteRequest>) {
        self.routes_tx = Some(tx);
    }

    /// Attach the device-watcher outlet: `Monitor`/`Forget` requests for
    /// TVs whose connector names a local device node flow to the daemon's
    /// device watcher, which answers with [`InEvent::DeviceAvailability`].
    /// With one attached, a local-device TV's first dial waits for the
    /// watcher's first report. Without one, no device monitoring: those
    /// TVs dial as network TVs without a route monitor do (tests).
    pub fn set_device_monitor(&mut self, tx: mpsc::UnboundedSender<DeviceRequest>) {
        self.devices_tx = Some(tx);
        self.world.resource_mut::<DeviceWatch>().0 = true;
    }

    /// Hand the system profile's sleep requests to `tx`: the daemon's
    /// logind adapter, which puts the machine to sleep.
    pub fn set_sleeper(&mut self, tx: mpsc::UnboundedSender<SleepRequest>) {
        self.sleep_tx = Some(tx);
    }

    /// Hand the wake list to `tx`: the device nodes of the TVs whose system
    /// profile lets them wake the PC, sent whenever the set changes (and
    /// once at the first step). The daemon writes it for the wakeup unit,
    /// which arms those adapters before each sleep.
    pub fn set_wake_list(&mut self, tx: mpsc::UnboundedSender<Vec<std::path::PathBuf>>) {
        self.wake_list_tx = Some(tx);
    }

    /// Apply the boot state before [`run`](Self::run): `stored` (the
    /// snapshot loaded from the store, if any) becomes the persist
    /// baseline, then `commands` apply as one synchronous step. Replaying
    /// the store writes nothing back, and no step ever runs against a
    /// half-replayed table. Attach the store and route outlets first, so
    /// a real difference (a migrated record, the default profile on a
    /// fresh store) and the boot dials reach them.
    pub fn seed(&mut self, stored: Option<StoreSnapshot>, commands: Vec<Command>) {
        self.world.resource_mut::<LastPersisted>().0 = stored;
        self.step(commands.into_iter().map(InEvent::Command).collect());
    }

    /// Per-vendor verify-retry tuning (from each backend's
    /// `verify_retry_config`); vendors without an entry get the default.
    pub fn set_vendor_retry(&mut self, retry: HashMap<String, tisvlay_core::RetryConfig>) {
        self.vendor_retry = retry;
    }

    /// Run until every inbox sender is dropped.
    pub async fn run(mut self) {
        let mut buffer: Vec<InEvent> = Vec::with_capacity(64);
        loop {
            let received = self.inbox_rx.recv_many(&mut buffer, 64).await;
            if received == 0 {
                return;
            }
            self.step(std::mem::take(&mut buffer));
        }
    }

    /// One sync point: apply a batch of events, run the schedule, execute
    /// drained effects.
    fn step(&mut self, batch: Vec<InEvent>) {
        tracing::trace!(batch = batch.len(), "step");
        let mut commands = Vec::new();
        let mut drains = Vec::new();
        for event in batch {
            match event {
                // Ticks apply first, so the batch's commands and facts all
                // see its clock.
                InEvent::Tick { now } => {
                    // Monotonic: a production ticker and test-injected
                    // future instants coexist without regressing time.
                    let mut clock = self.world.resource_mut::<Clock>();
                    clock.0 = clock.0.max(now);
                }
                InEvent::Command(command) => commands.push(command),
                // Held by value, not as a message: a message outlives the
                // step, and the token's drop is the signal.
                InEvent::Drain(token) => drains.push(token),
                fact => {
                    self.world.write_message(fact);
                }
            }
        }
        for command in commands {
            self.apply(command);
        }
        self.schedule.run(&mut self.world);
        self.world.resource_mut::<Messages<InEvent>>().update();
        // Ages out component-removal records, which otherwise accumulate
        // for the world's lifetime: no App drives this here.
        self.world.clear_trackers();
        #[cfg(debug_assertions)]
        assert_indexes_match(&mut self.world);

        let effects: Vec<Effect> = std::mem::take(&mut self.world.resource_mut::<Outbox>().0);
        for effect in effects {
            match effect {
                Effect::Wire(wire) => {
                    let _ = self.effects_tx.send(wire);
                }
                Effect::WakeList(nodes) => {
                    if let Some(tx) = &self.wake_list_tx {
                        let _ = tx.send(nodes);
                    }
                }
                Effect::Sleep(request) => match &self.sleep_tx {
                    Some(tx) => {
                        let _ = tx.send(request);
                    }
                    None => tracing::warn!(?request, "no sleeper attached, sleep dropped"),
                },
                Effect::ForgetMonitors { tv } => {
                    if let Some(tx) = &self.routes_tx {
                        let _ = tx.send(RouteRequest::Forget { tv });
                    }
                    if let Some(tx) = &self.devices_tx {
                        let _ = tx.send(DeviceRequest::Forget { tv });
                    }
                }
                Effect::SpawnConnect {
                    tv,
                    dial,
                    pairing,
                    vendor,
                    config,
                    wake,
                } => match self.connectors.get(&vendor) {
                    Some(connector) => {
                        // Re-evaluate the statics on every dial: this is
                        // where a merged config (update-config, re-pair)
                        // first reaches the reactor.
                        let traits = transport_facts(connector.as_ref(), &config);
                        let _ = self.inbox_tx.send(InEvent::TransportFacts {
                            tv,
                            supports_wake: connector.supports_wake(&config),
                            traits,
                            primary: connector.primary_descriptor(&config),
                            config_rows: connector.display_config(&config),
                            diagnostic_rows: connector.display_diagnostics(&config),
                            device: connector.local_device(&config).map(|device| device.path),
                        });
                        self.request_monitors(tv, connector.as_ref(), &config);
                        let retry = self.vendor_retry.get(&vendor).copied().unwrap_or_default();
                        tokio::spawn(backend::connect_flow(
                            tv,
                            dial,
                            pairing,
                            Arc::clone(connector),
                            config,
                            wake,
                            retry,
                            traits,
                            self.inbox_tx.clone(),
                        ));
                    }
                    None => {
                        // AddTv validates vendors, so this is unreachable in
                        // practice — surface it as a dial failure if it happens.
                        let _ = self.inbox_tx.send(InEvent::Backend {
                            tv,
                            dial,
                            event: BackendEvent::ConnectFailed {
                                reason: crate::events::DialFailure::message(format!(
                                    "no connector for vendor '{vendor}'"
                                )),
                            },
                        });
                    }
                },
                Effect::StartPairing { tv, vendor, params } => {
                    let update = match self.connectors.get(&vendor) {
                        Some(connector) => match connector.start_pairing(&params) {
                            Ok(session) => {
                                tokio::spawn(backend::pairing_task(
                                    tv,
                                    session,
                                    self.inbox_tx.clone(),
                                ));
                                continue;
                            }
                            Err(error) => {
                                PairingUpdate::State(Box::new(tisvlay_core::PairingState::failed(
                                    tisvlay_core::PairingError::connection_failed(
                                        error.to_string(),
                                    ),
                                )))
                            }
                        },
                        None => PairingUpdate::ConfigFailed(format!(
                            "no connector for vendor '{vendor}'"
                        )),
                    };
                    let _ = self
                        .inbox_tx
                        .send(InEvent::PairingUpdate { id: tv, update });
                }
                Effect::CompletePairing {
                    tv,
                    vendor,
                    params,
                    credentials,
                } => {
                    let update = match self.connectors.get(&vendor) {
                        Some(connector) => {
                            match connector.build_config_from_pairing(&params, &credentials) {
                                Ok(config) => PairingUpdate::ConfigReady(config),
                                Err(error) => PairingUpdate::ConfigFailed(error.to_string()),
                            }
                        }
                        None => PairingUpdate::ConfigFailed(format!(
                            "no connector for vendor '{vendor}'"
                        )),
                    };
                    let _ = self
                        .inbox_tx
                        .send(InEvent::PairingUpdate { id: tv, update });
                }
                Effect::Facts { tv, vendor, config } => {
                    if let Some(connector) = self.connectors.get(&vendor) {
                        let _ = self.inbox_tx.send(InEvent::TransportFacts {
                            tv,
                            supports_wake: connector.supports_wake(&config),
                            traits: transport_facts(connector.as_ref(), &config),
                            primary: connector.primary_descriptor(&config),
                            config_rows: connector.display_config(&config),
                            diagnostic_rows: connector.display_diagnostics(&config),
                            device: connector.local_device(&config).map(|device| device.path),
                        });
                        self.request_monitors(tv, connector.as_ref(), &config);
                    }
                }
                Effect::Persist(snapshot) => match &self.persist_tx {
                    Some(tx) => {
                        let _ = tx.send(StoreMsg::Persist(snapshot));
                    }
                    None => tracing::debug!("no store attached, persist snapshot dropped"),
                },
                Effect::StoreReloaded(applied) => {
                    if let Some(tx) = &self.persist_tx {
                        let _ = tx.send(StoreMsg::Reloaded(applied));
                    }
                }
            }
        }
        // Behind everything this step sent: a connection reaches the
        // token only after running the commands queued before it.
        if !drains.is_empty() {
            let mut handles = self.world.query::<&ConnectionHandle>();
            for handle in handles.iter(&self.world) {
                for token in &drains {
                    let _ = handle.cmd_tx.send(BackendCmd::Drain(token.clone()));
                }
            }
        }
    }

    /// Apply one client command: run its handler as a one-shot system
    /// and answer its reply. The handler's `Commands` flush on return, so
    /// the next command sees this one's effects. The match is exhaustive:
    /// a command without a handler doesn't compile.
    fn apply(&mut self, command: Command) {
        match command {
            Command::Pair {
                id,
                attempt,
                name,
                vendor,
                config,
                managed,
                reply,
            } => {
                let input = handle::PairInput {
                    id,
                    attempt,
                    name,
                    vendor,
                    config,
                    managed,
                };
                let result = self.run_handler(handle::pair, input);
                self.answer(reply, result);
            }
            Command::RePair { id, attempt, reply } => {
                let result = self.run_handler(handle::re_pair, (id, attempt));
                self.answer(reply, result);
            }
            Command::CancelPair { id, reply } => {
                let result = self.run_handler(handle::cancel_pair, id);
                self.answer(reply, result);
            }
            Command::AddTv { record, reply } => {
                let result = self.run_handler(handle::add_tv, *record);
                self.answer(reply, result);
            }
            Command::RemoveTv { id, reply } => {
                let result = self.run_handler(handle::remove_tv, id);
                self.answer(reply, result);
            }
            Command::Reconnect { id, reply } => {
                let result = self.run_handler(handle::reconnect, id);
                self.answer(reply, result);
            }
            Command::SetName { id, name, reply } => {
                let result = self.run_handler(handle::set_name, (id, name));
                self.answer(reply, result);
            }
            Command::SetManaged { id, managed, reply } => {
                let result = self.run_handler(handle::set_managed, (id, managed));
                self.answer(reply, result);
            }
            Command::SetSystemProfile { id, profile, reply } => {
                let result = self.run_handler(handle::set_system_profile, (id, profile));
                self.answer(reply, result);
            }
            Command::SetPrimaryInput { id, input, reply } => {
                let result = self.run_handler(handle::set_primary_input, (id, input));
                self.answer(reply, result);
            }
            Command::SetVolume { id, level, reply } => {
                let ack = reply.map(AckReply::from);
                let result = self.run_handler(
                    handle::backend_command,
                    (id, BackendCmd::SetVolume(level, ack)),
                );
                self.refuse(ack, result);
            }
            Command::SetMuted { id, muted, reply } => {
                let ack = reply.map(AckReply::from);
                let result = self.run_handler(
                    handle::backend_command,
                    (id, BackendCmd::SetMuted(muted, ack)),
                );
                self.refuse(ack, result);
            }
            Command::SwitchInput { id, input, reply } => {
                let ack = reply.map(AckReply::from);
                let result = self.run_handler(handle::switch_input, (id, input, ack));
                self.refuse(ack, result);
            }
            Command::SetBacklight { id, level, reply } => {
                let ack = reply.map(AckReply::from);
                let result = self.run_handler(
                    handle::backend_command,
                    (id, BackendCmd::SetBacklight(level, ack)),
                );
                self.refuse(ack, result);
            }
            Command::SetScreen {
                id,
                on,
                restore_input,
                reply,
            } => {
                let result = self.run_handler(handle::set_screen, (id, on, restore_input));
                self.answer(reply, result);
            }
            Command::PowerOn {
                id,
                restore_input,
                reply,
            } => {
                let result = self.run_handler(handle::power_on, (id, restore_input));
                self.answer(reply, result);
            }
            Command::PowerOff { id, reply } => {
                let ack = reply.map(AckReply::from);
                let result = self.run_handler(handle::power_off, (id, ack));
                self.refuse(ack, result);
            }
            Command::GetConfig { id, reply } => {
                // The reactor owns the connectors (and thus the schemas),
                // so Secret fields are stripped here and credentials never
                // reach the bridge.
                let result = self
                    .run_handler(handle::get_config, id)
                    .map(|record| redacted_config(&self.connectors, record));
                self.answer(reply, result);
            }
            Command::UpdateConfig { id, patch, reply } => {
                let result = self.run_handler(handle::update_config, (id, patch));
                self.answer(reply, result);
            }
            Command::AddProfile { record, reply } => {
                let result = self.run_handler(handle::add_profile, *record);
                self.answer(reply, result);
            }
            Command::UpdateProfile { id, body, reply } => {
                let result = self.run_handler(handle::update_profile, (id, *body));
                self.answer(reply, result);
            }
            Command::DuplicateProfile {
                id,
                new_id,
                name,
                reply,
            } => {
                let result = self.run_handler(handle::duplicate_profile, (id, new_id, name));
                self.answer(reply, result);
            }
            Command::DeleteProfile { id, reply } => {
                let result = self.run_handler(handle::delete_profile, id);
                self.answer(reply, result);
            }
            Command::ProfileDefaults { reply } => {
                self.answer(reply, Ok(handle::profile_defaults()));
            }
            Command::EffectiveProfile { id, reply } => {
                let result = self.run_handler(handle::effective_profile, id);
                self.answer(reply, result);
            }
            Command::ReloadStore { edit, reply } => {
                let result = match edit {
                    Ok(edit) => {
                        let disk = edit.disk.clone();
                        let result = self.run_handler(crate::systems::reload::reload_store, *edit);
                        // The store writer holds its writes until it hears
                        // the outcome; on success the file it read is its
                        // new record of what's on disk.
                        let applied = result.is_ok().then(|| Box::new(disk));
                        self.world
                            .resource_mut::<Outbox>()
                            .0
                            .push(Effect::StoreReloaded(applied));
                        result
                    }
                    Err(error) => Err(error),
                };
                self.answer(reply, result);
            }
        }
    }

    /// Run a command handler now, flushing its `Commands` before returning.
    fn run_handler<I, O, M>(
        &mut self,
        handler: impl IntoSystem<I, O, M> + 'static,
        input: I::Inner<'_>,
    ) -> O
    where
        I: SystemInput + 'static,
        O: 'static,
    {
        self.world
            .run_system_cached_with(handler, input)
            .expect("command handlers are fn items, cached on first use and never unregistered")
    }

    /// Answer a command's reply with its handler's result. Replies queue
    /// behind the handler's own effects. A refused command nobody asked
    /// for (the boot seed sends replyless ones) is logged instead.
    fn answer<R: Rpc>(&mut self, reply: Option<Reply<R>>, result: Result<R::Response, WireError>) {
        let effect = match (reply, result) {
            (Some(reply), Ok(response)) => reply.ok(&response),
            (Some(reply), Err(error)) => AnyReply::from(reply).err(error),
            (None, Ok(_)) => return,
            (None, Err(error)) => {
                tracing::warn!(rpc = R::NAME, code = ?error.code, message = %error.message, "command refused");
                return;
            }
        };
        self.world
            .resource_mut::<Outbox>()
            .0
            .push(Effect::Wire(effect));
    }

    /// Refuse a backend command its handler couldn't send. A sent one is
    /// acked from the connection task once the TV completes it.
    fn refuse(&mut self, reply: Option<AckReply>, result: Result<(), WireError>) {
        if let Err(error) = result {
            match reply {
                Some(reply) => self
                    .world
                    .resource_mut::<Outbox>()
                    .0
                    .push(Effect::Wire(AnyReply::from(reply).err(error))),
                None => {
                    tracing::warn!(code = ?error.code, message = %error.message, "backend command refused");
                }
            }
        }
    }

    /// Ask the watchers to monitor what this TV's connection depends on.
    /// The netlink monitor route-resolves and watches a network-dependent
    /// TV's address (it skips descriptors that don't parse as IPs, like
    /// mock addresses); the device watcher watches a local device node.
    /// Sent at enrollment and on every dial spawn, so a config change
    /// that moves the address or the node reaches both.
    fn request_monitors(&self, tv: uuid::Uuid, connector: &dyn TvConnector, config: &VendorConfig) {
        if let Some(tx) = &self.routes_tx
            && connector.transport().is_network_dependent()
            && let Some(host) = connector.primary_descriptor(config)
        {
            let _ = tx.send(RouteRequest::Monitor { tv, host });
        }
        if let Some(tx) = &self.devices_tx
            && let Some(device) = connector.local_device(config)
        {
            let _ = tx.send(DeviceRequest::Monitor {
                tv,
                path: device.path,
            });
        }
    }
}

/// The connector's config-derived transport characteristics.
fn transport_facts(
    connector: &dyn TvConnector,
    config: &VendorConfig,
) -> crate::events::TransportInfo {
    crate::events::TransportInfo {
        reachable_in_standby: connector.reachable_in_standby(config),
        network_dependent: connector.transport().is_network_dependent(),
        survives_ip_change: connector.survives_source_ip_change(),
        emulate_screen_via_power: connector.emulate_screen_via_power(config),
        local_device: connector.local_device(config).is_some(),
    }
}

/// Every hook-kept index maps exactly the live entities carrying its key
/// component, both ways. Checked after every step in debug builds (the
/// test suites included): a drifted index would misroute commands.
#[cfg(debug_assertions)]
fn assert_indexes_match(world: &mut World) {
    use crate::components::{Keyed, PairingFor, ProfileId, TvId, UuidMap};
    use std::ops::Deref;
    fn check<K: Keyed, I: Resource + Deref<Target = UuidMap>>(world: &mut World) {
        let live: HashMap<uuid::Uuid, Entity> = world
            .query::<(Entity, &K)>()
            .iter(world)
            .map(|(entity, key)| (key.key(), entity))
            .collect();
        let indexed = world.resource::<I>().snapshot();
        assert_eq!(
            indexed,
            live,
            "{} must index exactly the live {} entities",
            std::any::type_name::<I>(),
            std::any::type_name::<K>(),
        );
    }
    check::<TvId, TvIndex>(world);
    check::<ProfileId, ProfileIndex>(world);
    check::<PairingFor, PairingIndex>(world);
}

/// The world's fallback error handler: log through tracing with the
/// failing system's name, then defer to bevy's default. A caught panic
/// still resumes its unwind, so the reactor task dies and the daemon's
/// supervisor restarts the process.
fn log_world_error(error: BevyError, ctx: ErrorContext) {
    tracing::error!(kind = ctx.kind(), name = %ctx.name(), %error, "world error");
    match_severity(error, ctx);
}

/// Project a TV record into its wire-safe config reply: top-level fields
/// plus the vendor table minus `Secret`-visibility fields (per the vendor
/// schema). Credentials never leave the process.
fn redacted_config(
    connectors: &HashMap<String, Arc<dyn TvConnector>>,
    record: TvRecord,
) -> TvConfigView {
    let mut config = serde_json::Map::new();
    if let Some(connector) = connectors.get(&record.vendor) {
        let secret: HashSet<String> = connector
            .config_schema()
            .into_iter()
            .filter(|field| field.visibility == tisvlay_core::FieldVisibility::Secret)
            .map(|field| field.name)
            .collect();
        for (key, value) in record.config.iter() {
            if secret.contains(key) {
                continue;
            }
            if let Ok(json) = serde_json::to_value(value) {
                config.insert(key.to_string(), json);
            }
        }
    }
    TvConfigView {
        id: record.id,
        name: record.name,
        vendor: record.vendor,
        managed: record.managed,
        system_profile: record.system_profile,
        primary_input: record.primary_input,
        config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tisvlay_core::{FieldVisibility, PairingField};

    struct StubConnector;

    #[async_trait]
    impl TvConnector for StubConnector {
        fn transport(&self) -> tisvlay_core::Transport {
            tisvlay_core::Transport::Network
        }
        async fn connect(
            &self,
            _: &VendorConfig,
        ) -> tisvlay_core::Result<Box<dyn tisvlay_core::TvConnection>> {
            unreachable!("redaction test never connects")
        }
        fn supports_wake(&self, _: &VendorConfig) -> bool {
            false
        }
        async fn wake(&self, _: &VendorConfig) -> tisvlay_core::Result<()> {
            Ok(())
        }
        async fn wake_and_connect(
            &self,
            _: &VendorConfig,
        ) -> tisvlay_core::Result<Box<dyn tisvlay_core::TvConnection>> {
            unreachable!("redaction test never connects")
        }
        fn start_pairing(
            &self,
            _: &VendorConfig,
        ) -> tisvlay_core::Result<tisvlay_core::PairingSession> {
            unreachable!("redaction test never pairs")
        }
        fn build_config_from_pairing(
            &self,
            _: &VendorConfig,
            _: &tisvlay_core::Credentials,
        ) -> tisvlay_core::Result<VendorConfig> {
            unreachable!("redaction test never pairs")
        }
        fn config_schema(&self) -> Vec<PairingField> {
            vec![
                PairingField {
                    name: "ip".into(),
                    ..Default::default()
                },
                PairingField {
                    name: "client_key".into(),
                    visibility: FieldVisibility::Secret,
                    ..Default::default()
                },
            ]
        }
    }

    #[derive(bevy_ecs::component::Component)]
    struct Marker;

    #[test]
    fn error_handler_resumes_a_caught_panic() {
        let ctx = ErrorContext::System {
            name: "test_system".into(),
            last_run: bevy_ecs::change_detection::Tick::new(0),
        };
        let error = BevyError::panic("boom", Box::new("boom"));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            log_world_error(error, ctx);
        }));
        let payload = unwound.expect_err("a caught panic resumes unwinding");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"boom"));
    }

    /// Component removals are recorded for `RemovedComponents` readers;
    /// each step must age them out, or the record grows for the life of
    /// the daemon (markers come and go constantly).
    #[test]
    fn steps_age_out_removal_records() {
        let (mut reactor, _effects, _client) = Reactor::new("test", Vec::new());
        let entity = reactor.world.spawn_empty().id();
        for _ in 0..10 {
            reactor
                .world
                .entity_mut(entity)
                .insert(Marker)
                .remove::<Marker>();
            reactor.step(Vec::new());
        }
        let marker = reactor
            .world
            .component_id::<Marker>()
            .expect("inserting Marker registered it");
        let recorded = reactor
            .world
            .removed_components()
            .get(marker)
            .map_or(0, Messages::len);
        assert!(recorded <= 2, "{recorded} removal records retained");
    }

    #[test]
    fn redaction_strips_secret_fields() {
        let mut connectors: HashMap<String, Arc<dyn TvConnector>> = HashMap::new();
        connectors.insert("stub".into(), Arc::new(StubConnector));
        let mut table = VendorConfig::new();
        table.insert("ip", "192.0.2.1");
        table.insert("client_key", "SECRET");
        let record = TvRecord {
            id: uuid::Uuid::nil(),
            name: "T".into(),
            vendor: "stub".into(),
            managed: true,
            system_profile: None,
            primary_input: None,
            config: table,
        };
        let view = redacted_config(&connectors, record);
        assert_eq!(view.config["ip"], "192.0.2.1");
        assert!(!view.config.contains_key("client_key"));
        assert!(!serde_json::to_string(&view).unwrap().contains("SECRET"));
        assert_eq!(view.name, "T");
    }
}
