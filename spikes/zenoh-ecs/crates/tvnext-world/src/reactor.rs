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
    Clock, Effect, KnownVendors, Outbox, PairingIndex, Site, SystemProfile, TvIndex,
};
use crate::events::{BackendEvent, Command, InEvent, PairingUpdate, WireEffect};
use crate::systems::build_schedule;

pub struct Reactor {
    world: World,
    schedule: Schedule,
    inbox_rx: mpsc::UnboundedReceiver<InEvent>,
    inbox_tx: mpsc::UnboundedSender<InEvent>,
    connectors: HashMap<String, Arc<dyn TvConnector>>,
    effects_tx: mpsc::UnboundedSender<WireEffect>,
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
            },
            effects_rx,
            client,
        )
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
        for event in batch {
            match event {
                InEvent::Tick { now } => self.world.resource_mut::<Clock>().0 = now,
                other => {
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
                        tokio::spawn(backend::connect_flow(
                            tv,
                            Arc::clone(connector),
                            config,
                            wake,
                            self.inbox_tx.clone(),
                        ));
                    }
                    None => {
                        // AddTv validates vendors, so this is unreachable in
                        // practice — surface it as a dial failure if it happens.
                        let _ = self.inbox_tx.send(InEvent::Backend {
                            tv,
                            event: BackendEvent::ConnectFailed {
                                error: format!("no connector for vendor '{vendor}'"),
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
            }
        }
    }
}
