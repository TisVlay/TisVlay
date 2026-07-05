//! Reliable-dispatch tests: verify-retry-escalate for screen actions, and
//! the CEC-shaped transports (standby-reachable, screen-via-power).

mod support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::PowerState;
use tisvlay_world::{Command, InEvent, SystemEvent};

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
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "standby")
    })
    .await;
    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://cecprof"), 1);
}

/// The WoL-at-boot regression: a managed TV that is off at daemon start
/// (its plain replay dial fails and backs off) must still get woken by
/// the `Startup` dispatch — the stock `on_startup: power_on` upgrades
/// the pending plain retry to a fresh wake dial instead of waiting out
/// the backoff it can never win.
#[tokio::test]
async fn startup_event_upgrades_the_boot_dial_to_a_wake() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = uuid::Uuid::new_v4();
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String("mock://boot".into()));
    table.insert("refuse_connect".into(), toml::Value::Boolean(true));
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Boot", toml::Value::Table(table))),
        reply: None,
    });

    // The replay-shaped plain dial fails once and enters backoff.
    let failed = |e: &tisvlay_world::WireEffect| is_publish_on(e, "event/connect-failed").is_some();
    collect_until(&mut effects, failed).await;
    assert_eq!(handle.connect_call_count("mock://boot"), 1);

    // Startup dispatch: with no clock advance the backoff would hold,
    // so a second dial can only come from the wake upgrade.
    client.send(InEvent::System {
        event: SystemEvent::Startup,
        connector: None,
    });
    collect_until(&mut effects, failed).await;
    assert!(handle.connect_call_count("mock://boot") >= 2);
}

/// The reliable-screen elevation: `screen_on` against a disconnected
/// wake-capable TV rides the wake path (acked when the dial arms, like
/// `power-on`) instead of erroring `not-connected` — the session-unlock
/// regression where the TV stayed dark because the rpc bounced.
#[tokio::test]
async fn disconnected_screen_on_elevates_to_wake() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = uuid::Uuid::new_v4();
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String("mock://elev".into()));
    table.insert("refuse_connect".into(), toml::Value::Boolean(true));
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Elev", toml::Value::Table(table))),
        reply: None,
    });
    let failed = |e: &tisvlay_world::WireEffect| is_publish_on(e, "event/connect-failed").is_some();
    collect_until(&mut effects, failed).await;
    assert_eq!(handle.connect_call_count("mock://elev"), 1);

    // No clock advance: the backoff holds, so a second dial can only
    // come from the elevation's wake upgrade.
    client.command(Command::SetScreen {
        id,
        on: true,
        reply: Some(40),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 40)).await;
    assert!(
        seen.iter().all(|e| reply_err_code(e, 40).is_none()),
        "elevated screen_on must ack, not error: {seen:#?}"
    );
    collect_until(&mut effects, failed).await;
    assert!(handle.connect_call_count("mock://elev") >= 2);
}

/// `screen_off` on a disconnected TV is a satisfied no-op — the panel
/// already is dark — and must not dial anything.
#[tokio::test]
async fn disconnected_screen_off_is_a_satisfied_noop() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = uuid::Uuid::new_v4();
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String("mock://dark".into()));
    table.insert("refuse_connect".into(), toml::Value::Boolean(true));
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Dark", toml::Value::Table(table))),
        reply: None,
    });
    let failed = |e: &tisvlay_world::WireEffect| is_publish_on(e, "event/connect-failed").is_some();
    collect_until(&mut effects, failed).await;

    client.command(Command::SetScreen {
        id,
        on: false,
        reply: Some(41),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 41)).await;
    assert!(
        seen.iter().all(|e| reply_err_code(e, 41).is_none()),
        "disconnected screen_off must ack as a no-op: {seen:#?}"
    );
    assert_eq!(
        handle.connect_call_count("mock://dark"),
        1,
        "a satisfied no-op must not dial"
    );
}

/// The pre-flight wake, network shape: `screen_on` against a connected
/// TV whose cached power is wake-needy (LG's ActiveStandby ACKs screen
/// commands without ever lighting the panel) skips the verify-retry
/// loop and spends the dispatch's wake dial immediately — LG has no
/// in-band power_on, so the power path IS the teardown + WoL redial.
#[tokio::test]
async fn wake_needy_screen_on_preflights_to_the_wake_dial() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://pre").await;

    handle.inject_power_state("mock://pre", PowerState::ActiveStandby);
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "activestandby")
    })
    .await;

    client.command(Command::SetScreen {
        id,
        on: true,
        reply: Some(42),
    });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(
        seen.iter()
            .any(|e| is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")),
        "the wake reconnect must report the panel active: {seen:#?}"
    );
    assert_eq!(
        handle.screen_on_call_count("mock://pre"),
        0,
        "pre-flight must ride the power path, not the screen command"
    );
    assert_eq!(handle.connect_call_count("mock://pre"), 2);
}

/// The pre-flight wake, standby-reachable shape (a future CEC-like
/// backend without screen emulation): the bus stays live in standby, so
/// the pre-flight wakes in place — in-band power_on, no teardown.
#[tokio::test]
async fn wake_needy_screen_on_wakes_in_place_when_standby_reachable() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = uuid::Uuid::new_v4();
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String("mock://sr".into()));
    table.insert("reachable_in_standby".into(), toml::Value::Boolean(true));
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Sr", toml::Value::Table(table))),
        reply: Some(1),
    });
    collect_until(&mut effects, |e| link_connected(e, true)).await;

    handle.inject_power_state("mock://sr", PowerState::ActiveStandby);
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "activestandby")
    })
    .await;

    client.command(Command::SetScreen {
        id,
        on: true,
        reply: Some(44),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;
    assert_eq!(
        handle.screen_on_call_count("mock://sr"),
        0,
        "pre-flight must ride the power path, not the screen command"
    );
    assert_eq!(
        handle.connect_call_count("mock://sr"),
        1,
        "in-place wake needs no teardown"
    );
}

/// A `screen_off` arriving while a dial is in flight latches instead of
/// no-opping: the reconnect re-asserts it over the fresh link — the
/// locked-idle timer racing a boot wake must land, not be swallowed.
#[tokio::test]
async fn screen_off_during_pending_dial_lands_after_connect() {
    let (mut effects, client, handle) = spawn_reactor();
    handle.set_connect_delay(Duration::from_millis(400));
    let id = uuid::Uuid::new_v4();
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Race", mock_config("mock://race"))),
        reply: Some(1),
    });
    // The AddTv reply proves the spawn (and its PendingConnect) flushed;
    // the dial itself is still sleeping in the connect delay.
    collect_until(&mut effects, |e| reply_to(e, 1)).await;

    client.command(Command::SetScreen {
        id,
        on: false,
        reply: Some(45),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 45)).await;
    assert!(
        seen.iter().all(|e| reply_err_code(e, 45).is_none()),
        "latched screen_off must ack: {seen:#?}"
    );

    // Connect lands (panel up) → the latch re-asserts screen_off.
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;
    assert_eq!(handle.screen_off_call_count("mock://race"), 1);
    assert_eq!(handle.connect_call_count("mock://race"), 1);
}

/// The one-latch invariant across a wake teardown: an escalated
/// `screen_on` re-asserts exactly once on the reconnect, and a TV that
/// keeps ACKing dark gets one wake dial per dispatch — then gives up
/// loudly instead of ping-ponging wake dials forever.
#[tokio::test]
async fn escalation_reasserts_on_reconnect_then_gives_up() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://loop").await;

    // Panel dark in a non-wake-needy state (the verify path), firmware
    // ACKing screen_on without effect — and the post-wake mock comes up
    // dark too, so the re-assert can never verify.
    handle.inject_power_state("mock://loop", PowerState::ScreenOff);
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;
    handle.set_screen_on_noop("mock://loop", true);
    handle.set_initial_power_state_override("mock://loop", PowerState::ActiveStandby);

    let t0 = Instant::now();
    client.tick(t0);
    client.command(Command::SetScreen {
        id,
        on: true,
        reply: Some(43),
    });
    collect_until(&mut effects, |e| reply_to(e, 43)).await;

    // Spend the verify budget → escalate to the wake dial.
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(400));
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(800));
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    collect_until(&mut effects, |e| link_connected(e, true)).await;

    // The surviving verify re-asserts over the fresh link, retries
    // within a fresh budget, and — already escalated — gives up.
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(1600));
    tokio::time::sleep(Duration::from_millis(50)).await;
    client.tick(t0 + Duration::from_millis(2400));
    collect_until(&mut effects, |e| {
        is_publish_on(e, "event/dispatch-failed").is_some()
    })
    .await;
    assert_eq!(
        handle.connect_call_count("mock://loop"),
        2,
        "exactly one wake escalation per dispatch"
    );
    assert_eq!(
        handle.screen_on_call_count("mock://loop"),
        4,
        "send + retry on each side of the reconnect"
    );
}
