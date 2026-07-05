//! The reactor: single owner of the bevy world. Batches inbox events, runs
//! the schedule at sync points, and executes drained effects.

use bevy_ecs::message::Messages;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;
use tisvlay_core::TvConnector;
use tokio::sync::mpsc;

use crate::backend;
use crate::components::{
    Clock, DirtyStore, Effect, KnownVendors, Outbox, PairingIndex, ProfileIndex, Site,
    SystemProfile, TvIndex,
};
use crate::events::{
    BackendEvent, Command, InEvent, PairingUpdate, RouteRequest, StoreSnapshot, TvRecord,
    WireEffect,
};
use crate::systems::build_schedule;

pub struct Reactor {
    world: World,
    schedule: Schedule,
    inbox_rx: mpsc::UnboundedReceiver<InEvent>,
    inbox_tx: mpsc::UnboundedSender<InEvent>,
    connectors: HashMap<String, Arc<dyn TvConnector>>,
    effects_tx: mpsc::UnboundedSender<WireEffect>,
    persist_tx: Option<mpsc::UnboundedSender<StoreSnapshot>>,
    routes_tx: Option<mpsc::UnboundedSender<RouteRequest>>,
    vendor_retry: HashMap<String, tisvlay_core::RetryConfig>,
}

impl fmt::Debug for Reactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reactor").finish_non_exhaustive()
    }
}

/// Cloneable sender into the reactor inbox — the seam where zenoh RPC glue
/// (M2), watchers, and tests inject events.
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
    /// Returns the wire-effects receiver (zenoh writer in M2, test tap
    /// today) and a client for injecting events.
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
        world.init_resource::<TvIndex>();
        world.init_resource::<PairingIndex>();
        world.init_resource::<ProfileIndex>();
        world.init_resource::<DirtyStore>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<InEvent>>();

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
                vendor_retry: HashMap::new(),
            },
            effects_rx,
            client,
        )
    }

    /// Attach the store outlet: `Persist` effects (full TV-table
    /// snapshots) flow here. Without one they are dropped — fine for
    /// ephemeral worlds like tests.
    pub fn set_persist(&mut self, tx: mpsc::UnboundedSender<StoreSnapshot>) {
        self.persist_tx = Some(tx);
    }

    /// Attach the route-monitor outlet: `Monitor`/`Forget` requests for
    /// network-dependent TVs flow to the daemon's netlink watcher, which
    /// answers with [`InEvent::RouteResolved`]. Without one, no route
    /// monitoring — fine for worlds with no real network (tests).
    pub fn set_route_monitor(&mut self, tx: mpsc::UnboundedSender<RouteRequest>) {
        self.routes_tx = Some(tx);
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
        for event in batch {
            match event {
                InEvent::Tick { now } => {
                    // Monotonic: a production ticker and test-injected
                    // future instants coexist without regressing time.
                    let mut clock = self.world.resource_mut::<Clock>();
                    clock.0 = clock.0.max(now);
                }
                other => {
                    // Removal also ends route monitoring for the TV's
                    // address. (Transport facts need no enrollment
                    // intercept: every TV dials, and the SpawnConnect
                    // arm sends them ahead of each dial outcome.)
                    if let InEvent::Command(Command::RemoveTv { id, .. }) = &other
                        && let Some(tx) = &self.routes_tx
                    {
                        let _ = tx.send(RouteRequest::Forget { tv: *id });
                    }
                    self.world.write_message(other);
                }
            }
        }
        self.schedule.run(&mut self.world);
        self.world.resource_mut::<Messages<InEvent>>().update();

        let effects: Vec<Effect> = std::mem::take(&mut self.world.resource_mut::<Outbox>().0);
        for effect in effects {
            match effect {
                Effect::Wire(wire) => {
                    let _ = self.effects_tx.send(wire);
                }
                Effect::SpawnConnect {
                    tv,
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
                        });
                        self.request_route_monitor(tv, connector.as_ref(), &config);
                        let retry = self.vendor_retry.get(&vendor).copied().unwrap_or_default();
                        tokio::spawn(backend::connect_flow(
                            tv,
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
                        });
                        self.request_route_monitor(tv, connector.as_ref(), &config);
                    }
                }
                Effect::Persist(snapshot) => match &self.persist_tx {
                    Some(tx) => {
                        let _ = tx.send(*snapshot);
                    }
                    None => tracing::debug!("no store attached, persist snapshot dropped"),
                },
                Effect::RedactConfig { reply, record } => {
                    let payload = redacted_config(&self.connectors, record);
                    let _ = self.effects_tx.send(WireEffect::Reply {
                        to: reply,
                        result: Ok(payload),
                    });
                }
            }
        }
    }

    /// Ask the netlink monitor to route-resolve and watch this TV's
    /// address. Only network-dependent transports with a primary
    /// descriptor participate; the monitor itself skips descriptors
    /// that don't parse as IPs (mock addresses).
    fn request_route_monitor(
        &self,
        tv: uuid::Uuid,
        connector: &dyn TvConnector,
        config: &toml::Value,
    ) {
        let Some(tx) = &self.routes_tx else { return };
        if !connector.transport().is_network_dependent() {
            return;
        }
        if let Some(host) = connector.primary_descriptor(config) {
            let _ = tx.send(RouteRequest::Monitor { tv, host });
        }
    }
}

/// The connector's config-derived transport characteristics.
fn transport_facts(
    connector: &dyn TvConnector,
    config: &toml::Value,
) -> crate::events::TransportInfo {
    crate::events::TransportInfo {
        reachable_in_standby: connector.reachable_in_standby(config),
        network_dependent: connector.transport().is_network_dependent(),
        survives_ip_change: connector.survives_source_ip_change(),
        emulate_screen_via_power: connector.emulate_screen_via_power(config),
    }
}

/// Project a TV record into its wire-safe config reply: top-level fields
/// plus the vendor table minus `Secret`-visibility fields (per the vendor
/// schema). Credentials never leave the process.
fn redacted_config(
    connectors: &HashMap<String, Arc<dyn TvConnector>>,
    record: TvRecord,
) -> serde_json::Value {
    let mut config = serde_json::Map::new();
    if let Some(connector) = connectors.get(&record.vendor) {
        let secret: HashSet<String> = connector
            .config_schema()
            .into_iter()
            .filter(|field| field.visibility == tisvlay_core::FieldVisibility::Secret)
            .map(|field| field.name)
            .collect();
        if let Some(table) = record.config.as_table() {
            for (key, value) in table {
                if secret.contains(key) {
                    continue;
                }
                if let Ok(json) = serde_json::to_value(value) {
                    config.insert(key.clone(), json);
                }
            }
        }
    }
    serde_json::json!({
        "id": record.id,
        "name": record.name,
        "vendor": record.vendor,
        "managed": record.managed,
        "system_profile": record.system_profile,
        "primary_input": record.primary_input,
        "automate_when": record.automate_when,
        "config": config,
    })
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
            _: &toml::Value,
        ) -> tisvlay_core::Result<Box<dyn tisvlay_core::TvConnection>> {
            unimplemented!("redaction test never connects")
        }
        fn supports_wake(&self, _: &toml::Value) -> bool {
            false
        }
        async fn wake(&self, _: &toml::Value) -> tisvlay_core::Result<()> {
            Ok(())
        }
        async fn wake_and_connect(
            &self,
            _: &toml::Value,
        ) -> tisvlay_core::Result<Box<dyn tisvlay_core::TvConnection>> {
            unimplemented!("redaction test never connects")
        }
        fn start_pairing(
            &self,
            _: &toml::Value,
        ) -> tisvlay_core::Result<tisvlay_core::PairingSession> {
            unimplemented!("redaction test never pairs")
        }
        fn build_config_from_pairing(
            &self,
            _: &toml::Value,
            _: &tisvlay_core::Credentials,
        ) -> tisvlay_core::Result<toml::Value> {
            unimplemented!("redaction test never pairs")
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

    #[test]
    fn redaction_strips_secret_fields() {
        let mut connectors: HashMap<String, Arc<dyn TvConnector>> = HashMap::new();
        connectors.insert("stub".into(), Arc::new(StubConnector));
        let mut table = toml::value::Table::new();
        table.insert("ip".into(), toml::Value::String("192.0.2.1".into()));
        table.insert("client_key".into(), toml::Value::String("SECRET".into()));
        let record = TvRecord {
            id: uuid::Uuid::nil(),
            name: "T".into(),
            vendor: "stub".into(),
            managed: true,
            system_profile: None,
            primary_input: None,
            automate_when: "always".into(),
            config: toml::Value::Table(table),
        };
        let json = redacted_config(&connectors, record);
        assert_eq!(json["config"]["ip"], "192.0.2.1");
        assert!(json["config"].get("client_key").is_none());
        assert!(!json.to_string().contains("SECRET"));
        assert_eq!(json["name"], "T");
    }
}
