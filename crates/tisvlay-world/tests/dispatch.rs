//! Reliable-dispatch tests: verify-retry-escalate for screen actions, and
//! the CEC-shaped transports (standby-reachable, screen-via-power).

mod support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::{EventAction, PowerState, SessionAction, SystemProfileConfig};
use tisvlay_world::{Command, InEvent, ProfileRecord, SystemEvent};
use uuid::Uuid;

/// A gated version of one action: the stock verb with `while_remote =
/// false`, so it fires normally while unarmed and is suppressed while
/// armed.
fn gated(action: SessionAction) -> EventAction {
    EventAction::With {
        action,
        restore_input: false,
        while_remote: false,
    }
}

/// Add a system profile from a body, assign it to `tv`, return the id.
async fn assign_remote_profile(
    effects: &mut tokio::sync::mpsc::UnboundedReceiver<tisvlay_world::WireEffect>,
    client: &tisvlay_world::WorldClient,
    tv: Uuid,
    body: SystemProfileConfig,
) -> Uuid {
    let profile = Uuid::new_v4();
    client.command(Command::AddProfile {
        record: Box::new(ProfileRecord { id: profile, body }),
        reply: Some(90),
    });
    collect_until(effects, |e| reply_to(e, 90)).await;
    client.command(Command::SetSystemProfile {
        id: tv,
        profile: Some(profile),
        reply: Some(91),
    });
    collect_until(effects, |e| reply_to(e, 91)).await;
    profile
}

/// Mirror the session service's armed set into the world.
fn arm(client: &tisvlay_world::WorldClient, armed: &[Uuid]) {
    client.send(InEvent::RemoteSession {
        armed: armed.to_vec(),
    });
}

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
        restore_input: false,
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
        restore_input: false,
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
        restore_input: false,
        reply: Some(32),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "standby")
    })
    .await;

    client.command(Command::SetScreen {
        id,
        on: true,
        restore_input: false,
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
        restore_input: false,
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
        restore_input: false,
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
        restore_input: false,
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
        restore_input: false,
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
        restore_input: false,
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
        restore_input: false,
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

/// While a remote session arms the TV, a `while_remote = false` DPMS-on
/// action is suppressed: the panel, blanked by DPMS-off, stays dark
/// across the DPMS-on. Disarming lets the next DPMS-on light it normally.
#[tokio::test]
async fn while_remote_gate_suppresses_dpms_on() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://supp").await;
    assign_remote_profile(
        &mut effects,
        &client,
        id,
        SystemProfileConfig {
            name: "Remote".into(),
            on_dpms_on: Some(gated(SessionAction::ScreenOn)),
            ..Default::default()
        },
    )
    .await;

    // Blank via DPMS-off (unarmed, the normal screen_off), then arm.
    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;
    arm(&client, &[id]);

    // DPMS-on is suppressed: no power change follows. Prove it by
    // driving a disarm + second DPMS-on and catching the panel going
    // active only then — a suppressed first pass would have lit it.
    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
    });
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;

    arm(&client, &[]);
    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;
}

/// The gate reverses the review's never-gate rule for explicit events:
/// a `while_remote = false` on `on_sleep` suppresses the off-going
/// power-off while armed, even though the docs recommend leaving
/// sleep ungated. This proves the exemption list is gone — the world
/// honors the gate wherever it's set.
#[tokio::test]
async fn while_remote_gate_suppresses_a_formerly_exempt_sleep() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://sleep").await;
    assign_remote_profile(
        &mut effects,
        &client,
        id,
        SystemProfileConfig {
            name: "Remote".into(),
            on_sleep: Some(gated(SessionAction::PowerOff)),
            ..Default::default()
        },
    )
    .await;
    arm(&client, &[id]);

    // Sleep's gated power-off is suppressed while armed: the link stays
    // up. Prove it by disarming + re-sleeping and catching the teardown
    // only then.
    client.send(InEvent::System {
        event: SystemEvent::Sleep,
        connector: None,
    });
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, false)
    })
    .await;

    arm(&client, &[]);
    client.send(InEvent::System {
        event: SystemEvent::Sleep,
        connector: None,
    });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
}

/// Suppress-by-default: a stock action gates while armed. A profile
/// leaving `on_dpms_on` at the stock `screen_on` (gate implicitly
/// `false`) does NOT light the panel for an armed TV — the empty room
/// stays dark. Disarming lets the next DPMS-on light it normally.
#[tokio::test]
async fn while_remote_default_suppresses_dpms_on_while_armed() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://inherit").await;
    // No profile fields set — every event resolves to its stock action;
    // the wake-ish stock floor is gated by default.
    assign_remote_profile(
        &mut effects,
        &client,
        id,
        SystemProfileConfig {
            name: "Remote".into(),
            ..Default::default()
        },
    )
    .await;

    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;
    arm(&client, &[id]);

    // The stock DPMS-on is suppressed while armed: no wake follows.
    // Prove it by disarming + a second DPMS-on and catching the panel
    // going active only then.
    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
    });
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;

    arm(&client, &[]);
    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "active")
    })
    .await;
}

/// Floor exemption: the off-going stock actions fire while armed even
/// with no profile fields set — a blank is always safe to let through,
/// and the machine going away turns the TV off for the empty room.
/// Contrast `while_remote_default_suppresses_dpms_on_while_armed`, where
/// the wake-ish stock floor is gated.
#[tokio::test]
async fn stock_sleep_and_shutdown_fire_while_armed() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://floor").await;
    // No profile fields set: the off-going events resolve to their stock
    // actions with the floor gate `true`.
    assign_remote_profile(
        &mut effects,
        &client,
        id,
        SystemProfileConfig {
            name: "Remote".into(),
            ..Default::default()
        },
    )
    .await;
    arm(&client, &[id]);

    // The stock DPMS-off blanks the panel even while armed.
    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;

    // Sleep's stock power-off tears the link down even while armed.
    client.send(InEvent::System {
        event: SystemEvent::Sleep,
        connector: None,
    });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
}

/// The no-session invariant: an empty armed set gates nothing. A TV
/// whose profile gates DPMS-on off still wakes normally while unarmed —
/// a headless TV with no user session never has its wakes suppressed.
#[tokio::test]
async fn empty_armed_set_gates_nothing() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://none").await;
    assign_remote_profile(
        &mut effects,
        &client,
        id,
        SystemProfileConfig {
            name: "Remote".into(),
            on_dpms_on: Some(gated(SessionAction::ScreenOn)),
            ..Default::default()
        },
    )
    .await;
    // Never armed (an empty set is also the default).

    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
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
}

/// The armed projection: the mirrored set flips `meta.remote_armed` on
/// the published `meta/config` payload, and disarming clears it.
#[tokio::test]
async fn remote_armed_projection_tracks_the_set() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://proj").await;

    arm(&client, &[id]);
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/meta/config").is_some_and(|p| p["remote_armed"] == true)
    })
    .await;

    arm(&client, &[]);
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/meta/config").is_some_and(|p| p["remote_armed"] == false)
    })
    .await;
}

/// Primary-input restore: a restore-flagged automation wake lands the
/// TV on its configured primary input once live (the TV "comes up on
/// the wrong input" via static seeding — `initial_input` ≠
/// `primary_input`), and a manual power-on stays input-neutral.
#[tokio::test]
async fn automation_wake_restores_the_primary_input() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = uuid::Uuid::new_v4();
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String("mock://inp".into()));
    table.insert("initial_input".into(), toml::Value::String("HDMI_2".into()));
    let mut record = tv_record(id, "Inp", toml::Value::Table(table));
    record.primary_input = Some("HDMI_1".into());
    client.command(Command::AddTv {
        record: Box::new(record),
        reply: Some(1),
    });
    collect_until(&mut effects, |e| link_connected(e, true)).await;

    // Stock on_startup = power_on with restore: the drain compares the
    // mirrored input and switches to primary.
    client.send(InEvent::System {
        event: SystemEvent::Startup,
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/input").is_some_and(|p| p["current"] == "HDMI_1")
    })
    .await;

    // Manual actions stay input-neutral: switch away, then a plain
    // power-on must not drag the input back to primary.
    client.command(Command::SwitchInput {
        id,
        input: "HDMI_2".into(),
        reply: Some(50),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/input").is_some_and(|p| p["current"] == "HDMI_2")
    })
    .await;
    client.command(Command::PowerOn {
        id,
        restore_input: false,
        reply: Some(51),
    });
    collect_until(&mut effects, |e| reply_to(e, 51)).await;
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        is_publish_on(e, "/state/input").is_some_and(|p| p["current"] == "HDMI_1")
    })
    .await;
}
