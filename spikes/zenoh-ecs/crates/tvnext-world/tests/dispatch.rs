//! Reliable-dispatch tests: verify-retry-escalate for screen actions, and
//! the CEC-shaped transports (standby-reachable, screen-via-power).

mod support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::PowerState;
use tvnext_world::{Command, InEvent, SystemEvent};

fn cec_like_config(address: &str) -> toml::Value {
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String(address.into()));
    table.insert("reachable_in_standby".into(), toml::Value::Boolean(true));
    table.insert(
        "emulate_screen_via_power".into(),
        toml::Value::Boolean(true),
    );
    toml::Value::Table(table)
}

/// LG "Bug A" shape: `screen_on` ACKs but the panel stays dark. The
/// verify tier retries within the budget, then escalates to the wake path
/// (teardown + wake dial for a network transport).
#[tokio::test]
async fn screen_on_noop_retries_then_escalates_to_wake() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://esc").await;
    assert_eq!(handle.connect_call_count("mock://esc"), 1);

    // Panel dark, firmware ACKing screen_on without effect.
    handle.inject_power_state("mock://esc", PowerState::ScreenOff);
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;
    handle.set_screen_on_noop("mock://esc", true);

    let t0 = Instant::now();
    client.tick(t0);
    client.command(Command::SetScreen {
        id,
        on: true,
        reply: Some(30),
    });
    collect_until(&mut effects, |e| reply_to(e, 30)).await;

    // First verify window: not satisfied → retry (attempt 2).
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(400));
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Second verify window: budget spent → escalate to the wake dial.
    client.tick(t0 + Duration::from_millis(800));

    // Escalation tears down and redials; the fresh mock comes up active.
    // Publishes land in chain order (power before link), so the active
    // power sample arrives within the link-true collect's history.
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(
        seen.iter()
            .any(|e| { is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active") }),
        "reconnect must report the panel active: {seen:#?}"
    );
    assert_eq!(
        handle.screen_on_call_count("mock://esc"),
        2,
        "verify tier must have retried within the budget"
    );
    assert_eq!(
        handle.connect_call_count("mock://esc"),
        2,
        "escalation redials via the wake path"
    );
}

/// `screen_off` has no escalation: spend the budget, give up loudly.
#[tokio::test]
async fn screen_off_noop_gives_up_with_event() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://off").await;
    handle.set_screen_off_noop("mock://off", true);

    let t0 = Instant::now();
    client.tick(t0);
    client.command(Command::SetScreen {
        id,
        on: false,
        reply: Some(31),
    });
    collect_until(&mut effects, |e| reply_to(e, 31)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(400));
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(800));

    let seen = collect_until(&mut effects, |e| {
        is_publish_on(e, "event/dispatch-failed").is_some()
    })
    .await;
    let failed = seen
        .iter()
        .find_map(|e| is_publish_on(e, "event/dispatch-failed"))
        .unwrap();
    assert_eq!(failed["action"], "screen_off");
    assert_eq!(handle.screen_off_call_count("mock://off"), 2);
    assert_eq!(
        handle.connect_call_count("mock://off"),
        1,
        "no teardown for a failed screen_off"
    );
}

/// CEC shape: standby-reachable + screen-via-power. Screen actions become
/// power actions, the link never drops (Connected ≠ power), and wake
/// happens in place on the live handle.
#[tokio::test]
async fn emulated_screen_keeps_standby_link_up() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = {
        let id = uuid::Uuid::new_v4();
        client.command(Command::AddTv {
            record: Box::new(tv_record(id, "Cec", cec_like_config("mock://cec"))),
            reply: Some(1),
        });
        collect_until(&mut effects, |e| link_connected(e, true)).await;
        id
    };

    client.command(Command::SetScreen {
        id,
        on: false,
        reply: Some(32),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "standby")
    })
    .await;

    client.command(Command::SetScreen {
        id,
        on: true,
        reply: Some(33),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;

    // The whole exchange rode one connection: no drop, no redial.
    assert_eq!(handle.connect_call_count("mock://cec"), 1);
    assert_quiet(&mut effects, Duration::from_millis(200), |e| {
        link_connected(e, false)
    })
    .await;
}

/// Profile dispatch honors the emulation too: DPMS-off powers a CEC-shaped
/// TV down in place.
#[tokio::test]
async fn profile_screen_actions_respect_emulation() {
    let (mut effects, client, handle) = spawn_reactor();
    client.command(Command::AddTv {
        record: Box::new(tv_record(
            uuid::Uuid::new_v4(),
            "Cec",
            cec_like_config("mock://cecprof"),
        )),
        reply: Some(1),
    });
    collect_until(&mut effects, |e| link_connected(e, true)).await;

    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "standby")
    })
    .await;
    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://cecprof"), 1);
}
