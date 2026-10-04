//! Shared helpers for the world integration tests: reactor bootstrap with the
//! mock backend, and effect-tap assertion utilities.

use std::sync::Arc;
use std::time::Duration;

use tisvlay_mock::{MockHandle, new_test_parts};
use tisvlay_world::{Command, Reactor, Reply, TvRecord, WireEffect, WorldClient};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

pub(crate) fn mock_config(address: &str) -> toml::Value {
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String(address.into()));
    toml::Value::Table(table)
}

pub(crate) fn tv_record(id: Uuid, name: &str, config: toml::Value) -> TvRecord {
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
    // RUST_LOG-driven diagnostics for failing runs; idempotent.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let (connector, _discovery, handle) = new_test_parts();
    let (reactor, effects, client) = Reactor::new("test", vec![("mock", Arc::new(connector))]);
    tokio::spawn(reactor.run());
    (effects, client, handle)
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

pub(crate) fn reply_err_code(effect: &WireEffect, to: u64) -> Option<&str> {
    match effect {
        WireEffect::Reply {
            to: t,
            result: Err(error),
        } if *t == to => Some(error.code.as_str()),
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
