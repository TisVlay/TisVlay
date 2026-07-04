//! Headless world tests: drive the reactor with commands + the mock
//! backend's test handle, assert on the wire-effects tap. The wire IS the
//! observable contract — no reaching into world internals.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tisvlay_core::PowerState;
use tisvlay_mock::{MockHandle, new_test_parts};
use tokio::sync::mpsc::UnboundedReceiver;
use tvnext_world::{BackendEvent, Command, InEvent, Reactor, WireEffect, WorldClient};
use uuid::Uuid;

fn mock_config(address: &str) -> toml::Value {
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String(address.into()));
    toml::Value::Table(table)
}

fn spawn_reactor() -> (UnboundedReceiver<WireEffect>, WorldClient, MockHandle) {
    let (connector, _discovery, handle) = new_test_parts();
    let (reactor, effects, client) = Reactor::new("test", vec![("mock", Arc::new(connector))]);
    tokio::spawn(reactor.run());
    (effects, client, handle)
}

/// Receive effects until `stop` matches, returning everything seen
/// (including the matching effect). Panics after 5 s.
async fn collect_until(
    rx: &mut UnboundedReceiver<WireEffect>,
    stop: impl Fn(&WireEffect) -> bool,
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
async fn assert_quiet(
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

fn is_publish_on<'a>(effect: &'a WireEffect, suffix: &str) -> Option<&'a serde_json::Value> {
    match effect {
        WireEffect::Publish { key, payload } if key.ends_with(suffix) => Some(payload),
        _ => None,
    }
}

fn link_connected(effect: &WireEffect, connected: bool) -> bool {
    is_publish_on(effect, "/state/link").is_some_and(|p| p["connected"] == connected)
}

fn reply_to(effect: &WireEffect, to: u64) -> bool {
    matches!(effect, WireEffect::Reply { to: t, .. } if *t == to)
}

fn reply_err_code(effect: &WireEffect, to: u64) -> Option<&str> {
    match effect {
        WireEffect::Reply {
            to: t,
            result: Err(error),
        } if *t == to => Some(&error.code),
        _ => None,
    }
}

async fn add_connected_tv(
    effects: &mut UnboundedReceiver<WireEffect>,
    client: &WorldClient,
    address: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    client.command(Command::AddTv {
        id,
        name: "Den".into(),
        vendor: "mock".into(),
        config: mock_config(address),
        managed: true,
        reply: Some(1),
    });
    collect_until(effects, |e| link_connected(e, true)).await;
    id
}

#[tokio::test]
async fn add_tv_connects_and_publishes_initial_state() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    client.command(Command::AddTv {
        id,
        name: "Den".into(),
        vendor: "mock".into(),
        config: mock_config("mock://den"),
        managed: true,
        reply: Some(1),
    });

    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;

    assert!(
        seen.iter().any(|e| matches!(
            e,
            WireEffect::Reply {
                to: 1,
                result: Ok(_)
            }
        )),
        "AddTv must reply ok; saw {seen:#?}"
    );
    let identity = seen
        .iter()
        .find_map(|e| is_publish_on(e, "/meta/identity"))
        .expect("identity published");
    assert_eq!(identity["name"], "Den");
    assert_eq!(identity["vendor"], "mock");
    let meta = seen
        .iter()
        .find_map(|e| is_publish_on(e, "/meta/config"))
        .expect("meta published");
    assert_eq!(meta["managed"], true);
    let volume = seen
        .iter()
        .find_map(|e| is_publish_on(e, "/state/volume"))
        .expect("volume published");
    assert_eq!(volume["level"], 30, "mock default volume");
    let power = seen
        .iter()
        .find_map(|e| is_publish_on(e, "/state/power"))
        .expect("power published");
    assert_eq!(power["is_on"], true);
    let key_prefix = format!("tisvlay/test/tv/{id}/");
    assert!(
        seen.iter().all(|e| match e {
            WireEffect::Publish { key, .. } => key.starts_with(&key_prefix),
            _ => true,
        }),
        "all publishes live under the TV's key prefix"
    );
}

#[tokio::test]
async fn backend_push_becomes_state_publish() {
    let (mut effects, client, handle) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://push").await;

    handle.inject_power_state("mock://push", PowerState::ScreenOff);

    let seen = collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["is_screen_on"] == false)
    })
    .await;
    let power = seen
        .iter()
        .rev()
        .find_map(|e| is_publish_on(e, "/state/power"))
        .expect("power publish");
    assert_eq!(power["is_on"], true, "ScreenOff is still on");
    assert_eq!(power["state"], "screenoff");
}

/// The M1 exit criterion: remove-during-in-flight is a boring test.
/// Removal despawns at a sync point; late commands and stale backend events
/// for the dead uuid just miss the index — typed error out, silent drop in.
#[tokio::test]
async fn remove_tv_is_boring() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://gone").await;

    client.command(Command::RemoveTv { id, reply: Some(2) });
    let seen = collect_until(&mut effects, |e| reply_to(e, 2)).await;
    let deletes: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            WireEffect::Delete { key } => Some(key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deletes.len(), 5, "one DELETE per per-TV key: {deletes:?}");
    assert!(deletes.iter().all(|k| k.contains(&id.to_string())));

    // A command against the dead uuid gets a typed error, not a hang.
    client.command(Command::SetVolume {
        id,
        level: 10,
        reply: Some(3),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 3)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 3)),
        Some("tv-not-found")
    );

    // A stale backend event for the dead uuid is dropped silently — no
    // publish, no panic, reactor keeps serving.
    client.send(InEvent::Backend {
        tv: id,
        event: BackendEvent::State(Box::new(tisvlay_core::TvState::new())),
    });
    client.command(Command::SetVolume {
        id,
        level: 11,
        reply: Some(4),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 4)).await;
    assert!(
        seen.iter()
            .all(|e| !matches!(e, WireEffect::Publish { .. })),
        "stale event must not publish: {seen:#?}"
    );
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 4)),
        Some("tv-not-found")
    );
}

/// A disconnect (power_off on a network-style transport) stays down — no
/// auto-redial fighting a TV the user turned off. `Reconnect` brings it
/// back on request.
#[tokio::test]
async fn disconnect_stays_down_until_reconnect_command() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://den").await;
    assert_eq!(handle.connect_call_count("mock://den"), 1);

    client.command(Command::PowerOff { id, reply: None });
    collect_until(&mut effects, |e| link_connected(e, false)).await;

    // Even with time advancing well past any backoff, no dial happens.
    client.tick(Instant::now() + Duration::from_secs(60));
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, true)
    })
    .await;
    assert_eq!(
        handle.connect_call_count("mock://den"),
        1,
        "no auto-redial after disconnect"
    );

    client.command(Command::Reconnect { id, reply: Some(5) });
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.connect_call_count("mock://den"), 2);
}

/// Failed dials back off and give up after the attempt budget — observable
/// as `event/connect-failed` publishes gated by world time.
#[tokio::test]
async fn connect_failure_backs_off_and_gives_up() {
    let (mut effects, client, _handle) = spawn_reactor();
    let base = Instant::now();

    // An `address` of the wrong TOML type makes the mock connector's config
    // parse fail — the only reliable dial-failure knob the mock offers.
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::Integer(42));
    client.command(Command::AddTv {
        id: Uuid::new_v4(),
        name: "Broken".into(),
        vendor: "mock".into(),
        config: toml::Value::Table(table),
        managed: true,
        reply: None,
    });

    let failed = |e: &WireEffect| is_publish_on(e, "event/connect-failed").is_some();
    let seen = collect_until(&mut effects, failed).await;
    let attempt = seen
        .iter()
        .find_map(|e| is_publish_on(e, "event/connect-failed"))
        .unwrap();
    assert_eq!(attempt["attempt"], 1);

    // Backoff holds until world time advances.
    assert_quiet(&mut effects, Duration::from_millis(300), failed).await;

    client.tick(base + Duration::from_secs(4));
    let seen = collect_until(&mut effects, failed).await;
    assert_eq!(
        seen.iter()
            .find_map(|e| is_publish_on(e, "event/connect-failed"))
            .unwrap()["attempt"],
        2
    );

    client.tick(base + Duration::from_secs(8));
    let seen = collect_until(&mut effects, failed).await;
    assert_eq!(
        seen.iter()
            .find_map(|e| is_publish_on(e, "event/connect-failed"))
            .unwrap()["attempt"],
        3
    );

    // Attempt budget spent: further time advances dial nothing.
    client.tick(base + Duration::from_secs(20));
    assert_quiet(&mut effects, Duration::from_millis(300), failed).await;
}
