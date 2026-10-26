//! Shared helpers for the world integration tests: reactor bootstrap with the
//! mock backend, and effect-tap assertion utilities.

use std::sync::Arc;
use std::time::Duration;

use tisvlay_core::VendorConfig;
use tisvlay_mock::{MockHandle, new_test_parts};
use tisvlay_protocol::error::ErrorCode;
use tisvlay_world::{
    Command, DeviceRequest, Reactor, Reply, RouteRequest, StoreMsg, StoreSnapshot, TvRecord,
    WireEffect, WorldClient,
};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

pub(crate) fn mock_config(address: &str) -> VendorConfig {
    let mut config = VendorConfig::new();
    config.insert("address", address);
    config
}

pub(crate) fn tv_record(id: Uuid, name: &str, config: VendorConfig) -> TvRecord {
    TvRecord {
        id,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        system_profile: None,
        primary_input: None,
        config,
    }
}

pub(crate) fn spawn_reactor() -> (UnboundedReceiver<WireEffect>, WorldClient, MockHandle) {
    let (reactor, effects, client, handle) = new_reactor();
    tokio::spawn(reactor.run());
    (effects, client, handle)
}

/// [`spawn_reactor`] with the sleeper attached: the system profile's sleep
/// requests arrive on the returned receiver.
pub(crate) fn spawn_reactor_with_sleeper() -> (
    UnboundedReceiver<WireEffect>,
    UnboundedReceiver<tisvlay_world::SleepRequest>,
    WorldClient,
    MockHandle,
) {
    let (mut reactor, effects, client, handle) = new_reactor();
    let (sleep_tx, sleep_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.set_sleeper(sleep_tx);
    tokio::spawn(reactor.run());
    (effects, sleep_rx, client, handle)
}

/// [`spawn_reactor`] with the wake list tapped: each new list arrives on
/// the returned receiver.
pub(crate) fn spawn_reactor_with_wake_list() -> (
    UnboundedReceiver<WireEffect>,
    UnboundedReceiver<Vec<std::path::PathBuf>>,
    WorldClient,
    MockHandle,
) {
    let (mut reactor, effects, client, handle) = new_reactor();
    let (wake_tx, wake_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.set_wake_list(wake_tx);
    tokio::spawn(reactor.run());
    (effects, wake_rx, client, handle)
}

/// [`spawn_reactor`] with the store outlet tapped: every `Persist`
/// snapshot arrives on the returned receiver.
pub(crate) fn spawn_reactor_with_store() -> (
    UnboundedReceiver<WireEffect>,
    UnboundedReceiver<StoreMsg>,
    WorldClient,
    MockHandle,
) {
    let (mut reactor, effects, client, handle) = new_reactor();
    let (persist_tx, persist_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.set_persist(persist_tx);
    tokio::spawn(reactor.run());
    (effects, persist_rx, client, handle)
}

/// [`spawn_reactor_with_store`] booted the way the daemon boots: `stored`
/// is the persist baseline and `commands` apply before the reactor runs;
/// what `queue` sends then lands in the reactor's first batch.
pub(crate) fn spawn_seeded_reactor(
    stored: StoreSnapshot,
    commands: Vec<Command>,
    queue: impl FnOnce(&WorldClient),
) -> (
    UnboundedReceiver<WireEffect>,
    UnboundedReceiver<StoreMsg>,
    WorldClient,
    MockHandle,
) {
    let (mut reactor, effects, client, handle) = new_reactor();
    let (persist_tx, persist_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.set_persist(persist_tx);
    reactor.seed(Some(stored), commands);
    queue(&client);
    tokio::spawn(reactor.run());
    (effects, persist_rx, client, handle)
}

/// Wait until every effect of the steps before now has gone out: a
/// command sent now lands in a later step, so its reply comes after.
pub(crate) async fn settle(
    effects: &mut UnboundedReceiver<WireEffect>,
    client: &WorldClient,
    barrier: u64,
) {
    client.command(Command::SetVolume {
        id: Uuid::new_v4(),
        level: 0,
        reply: Some(Reply::new(barrier)),
    });
    collect_until(effects, |e| reply_to(e, barrier)).await;
}

/// [`spawn_reactor`] with the route-monitor outlet tapped.
pub(crate) fn spawn_reactor_with_routes() -> (
    UnboundedReceiver<WireEffect>,
    UnboundedReceiver<RouteRequest>,
    WorldClient,
    MockHandle,
) {
    let (mut reactor, effects, client, handle) = new_reactor();
    let (routes_tx, routes_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.set_route_monitor(routes_tx);
    tokio::spawn(reactor.run());
    (effects, routes_rx, client, handle)
}

/// [`spawn_reactor`] with the device-watcher outlet tapped, so
/// local-device TVs wait for availability reports the test sends.
pub(crate) fn spawn_reactor_with_devices() -> (
    UnboundedReceiver<WireEffect>,
    UnboundedReceiver<DeviceRequest>,
    WorldClient,
    MockHandle,
) {
    let (mut reactor, effects, client, handle) = new_reactor();
    let (devices_tx, devices_rx) = tokio::sync::mpsc::unbounded_channel();
    reactor.set_device_monitor(devices_tx);
    tokio::spawn(reactor.run());
    (effects, devices_rx, client, handle)
}

/// [`spawn_reactor`] with `queue` run before the reactor starts: what it
/// sends lands in the reactor's first batch (up to 64 events), so
/// same-batch interactions are deterministic.
pub(crate) fn spawn_reactor_after(
    queue: impl FnOnce(&WorldClient),
) -> (UnboundedReceiver<WireEffect>, WorldClient, MockHandle) {
    let (reactor, effects, client, handle) = new_reactor();
    queue(&client);
    tokio::spawn(reactor.run());
    (effects, client, handle)
}

fn new_reactor() -> (
    Reactor,
    UnboundedReceiver<WireEffect>,
    WorldClient,
    MockHandle,
) {
    // RUST_LOG-driven diagnostics for failing runs; idempotent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let (connector, _discovery, handle) = new_test_parts();
    let (reactor, effects, client) = Reactor::new("test", vec![("mock", Arc::new(connector))]);
    (reactor, effects, client, handle)
}

/// Every snapshot persisted so far, without waiting for more.
pub(crate) fn drain_snapshots(rx: &mut UnboundedReceiver<StoreMsg>) -> Vec<StoreSnapshot> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|msg| match msg {
            StoreMsg::Persist(snapshot) => Some(*snapshot),
            StoreMsg::Reloaded(_) => None,
        })
        .collect()
}

/// Receive effects until `stop` matches, returning everything seen
/// (including the matching effect). Panics after 5 s.
pub(crate) async fn collect_until(
    rx: &mut UnboundedReceiver<WireEffect>,
    mut stop: impl FnMut(&WireEffect) -> bool,
) -> Vec<WireEffect> {
    let mut seen = Vec::new();
    loop {
        let effect = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for effect; saw: {seen:#?}"))
            .expect("effects channel closed");
        let done = stop(&effect);
        seen.push(effect);
        if done {
            return seen;
        }
    }
}

/// Assert nothing matching `pred` arrives within `window`.
pub(crate) async fn assert_quiet(
    rx: &mut UnboundedReceiver<WireEffect>,
    window: Duration,
    pred: impl Fn(&WireEffect) -> bool,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => return,
            Ok(None) => panic!("effects channel closed"),
            Ok(Some(effect)) => {
                assert!(
                    !pred(&effect),
                    "unexpected effect in quiet window: {effect:?}"
                );
            }
        }
    }
}

pub(crate) fn is_publish_on<'a>(
    effect: &'a WireEffect,
    suffix: &str,
) -> Option<&'a serde_json::Value> {
    match effect {
        WireEffect::Publish { key, payload } if key.ends_with(suffix) => Some(payload),
        _ => None,
    }
}

pub(crate) fn link_connected(effect: &WireEffect, connected: bool) -> bool {
    is_publish_on(effect, "/state/link").is_some_and(|p| p["connected"] == connected)
}

pub(crate) fn reply_to(effect: &WireEffect, to: u64) -> bool {
    matches!(effect, WireEffect::Reply { to: t, .. } if *t == to)
}

pub(crate) fn reply_err_code(effect: &WireEffect, to: u64) -> Option<ErrorCode> {
    match effect {
        WireEffect::Reply {
            to: t,
            result: Err(error),
        } if *t == to => Some(error.code),
        _ => None,
    }
}

pub(crate) async fn add_connected_tv(
    effects: &mut UnboundedReceiver<WireEffect>,
    client: &WorldClient,
    address: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Den", mock_config(address))),
        reply: Some(Reply::new(1)),
    });
    collect_until(effects, |e| link_connected(e, true)).await;
    id
}

/// Give `tv` a new system profile with `body`.
pub(crate) async fn assign_system_profile(
    effects: &mut UnboundedReceiver<WireEffect>,
    client: &WorldClient,
    tv: Uuid,
    body: tisvlay_core::SystemProfileConfig,
) {
    let profile = Uuid::new_v4();
    client.command(Command::AddProfile {
        record: Box::new(tisvlay_world::ProfileRecord { id: profile, body }),
        reply: Some(Reply::new(90)),
    });
    collect_until(effects, |e| reply_to(e, 90)).await;
    client.command(Command::SetSystemProfile {
        id: tv,
        profile: Some(profile),
        reply: Some(Reply::new(91)),
    });
    collect_until(effects, |e| reply_to(e, 91)).await;
}
