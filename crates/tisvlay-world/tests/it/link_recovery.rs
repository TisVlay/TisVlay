//! A network TV's link dropping while the TV stays on: a router reboot or a
//! Wi-Fi outage, not a standby. The daemon redials such a TV until the
//! network is back, and restores the screen state last asked of it, which
//! the TV may have drifted from while nobody could see it. A drop the TV
//! announced (a standby report first) or one the user asked for (a power
//! off) still stays down.

use crate::support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::PowerState;
use tisvlay_world::{Command, Reply};

/// Past the redial backoff's 60 s cap, so each tick lets exactly one more
/// dial through whatever the attempt count.
const PAST_THE_CAP: Duration = Duration::from_secs(61);

fn power_state_is(effect: &tisvlay_world::WireEffect, state: &str) -> bool {
    is_publish_on(effect, "/state/power").is_some_and(|p| p["state"] == state)
}

/// Cut the link of a TV that stays on, with the network down behind it,
/// and wait for the world to see the drop.
/// Let the world check that a screen request landed (its verify window is
/// 300 ms of world time), so the request counts as settled, not in flight.
async fn settle_verify(client: &tisvlay_world::WorldClient, now: Instant) {
    client.tick(now);
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn outage(
    effects: &mut tokio::sync::mpsc::UnboundedReceiver<tisvlay_world::WireEffect>,
    handle: &tisvlay_mock::MockHandle,
    address: &str,
) {
    handle.set_unreachable(address, true);
    handle.cut_link(address);
    collect_until(effects, |e| link_connected(e, false)).await;
}

#[tokio::test]
#[ignore = "fails until a network TV redials after a drop it didn't announce"]
async fn a_drop_while_awake_redials_until_the_network_is_back() {
    let (mut effects, client, handle) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://outage").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://outage").await;
    // The outage outlasts the three-attempt budget: the dials keep coming.
    wait_for_dials(&handle, "mock://outage", 2).await;
    for k in 1..=5 {
        client.tick(t0 + PAST_THE_CAP * k);
        wait_for_dials(&handle, "mock://outage", 2 + k as usize).await;
    }

    handle.set_unreachable("mock://outage", false);
    client.tick(t0 + PAST_THE_CAP * 6);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
}

/// The LG shape of a remote-off: the TV reports a standby state, then
/// closes the link. That drop was announced, so nothing redials it.
#[tokio::test]
async fn a_drop_after_a_standby_report_stays_down() {
    let (mut effects, client, handle) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://remote-off").await;
    let t0 = Instant::now();
    client.tick(t0);

    handle.inject_power_state("mock://remote-off", PowerState::ActiveStandby);
    collect_until(&mut effects, |e| power_state_is(e, "activestandby")).await;
    handle.cut_link("mock://remote-off");
    collect_until(&mut effects, |e| link_connected(e, false)).await;

    for k in 1..=3 {
        client.tick(t0 + PAST_THE_CAP * k);
    }
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, true)
    })
    .await;
    assert_eq!(
        handle.connect_call_count("mock://remote-off"),
        1,
        "no redial after an announced standby"
    );
}

#[tokio::test]
#[ignore = "fails until a network TV redials after a drop it didn't announce"]
async fn a_power_off_while_redialing_ends_the_redial() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://give-up").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://give-up").await;
    wait_for_dials(&handle, "mock://give-up", 2).await;

    client.command(Command::PowerOff {
        id,
        reply: Some(Reply::new(51)),
    });
    collect_until(&mut effects, |e| reply_to(e, 51)).await;
    let dials = handle.connect_call_count("mock://give-up");
    for k in 1..=3 {
        client.tick(t0 + PAST_THE_CAP * k);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        handle.connect_call_count("mock://give-up"),
        dials,
        "a power-off ends the redial"
    );
}

/// The night that found this: a session event asked for the screen off
/// while the link was down after an outage. It must land once the TV is
/// back, not be acked as already satisfied.
#[tokio::test]
#[ignore = "fails until a network TV redials after a drop it didn't announce"]
async fn a_screen_off_while_down_after_a_drop_lands_on_reconnect() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://held").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://held").await;
    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(41)),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 41)).await;
    assert!(
        seen.iter().all(|e| reply_err_code(e, 41).is_none()),
        "the screen-off acks while it waits for the link: {seen:#?}"
    );

    handle.set_unreachable("mock://held", false);
    client.tick(t0 + PAST_THE_CAP);
    collect_until(&mut effects, |e| power_state_is(e, "screenoff")).await;
    assert_eq!(handle.screen_off_call_count("mock://held"), 1);
}

/// The TV was blanked, the link dropped, and the TV left screen-off on its
/// own while nobody could see it (LG's screen saver over the PC's input).
/// The reconnect puts the panel back the way it was last asked to be.
#[tokio::test]
#[ignore = "fails until a network TV redials after a drop it didn't announce"]
async fn the_reconnect_after_a_drop_restores_the_requested_screen_off() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://drift").await;
    let t0 = Instant::now();
    client.tick(t0);

    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(31)),
    });
    collect_until(&mut effects, |e| power_state_is(e, "screenoff")).await;
    assert_eq!(handle.screen_off_call_count("mock://drift"), 1);
    settle_verify(&client, t0 + Duration::from_secs(1)).await;

    handle.set_initial_power_state_override("mock://drift", PowerState::ScreenSaver);
    outage(&mut effects, &handle, "mock://drift").await;
    handle.set_unreachable("mock://drift", false);
    client.tick(t0 + PAST_THE_CAP);

    collect_until(&mut effects, |e| power_state_is(e, "screensaver")).await;
    collect_until(&mut effects, |e| power_state_is(e, "screenoff")).await;
    assert_eq!(
        handle.screen_off_call_count("mock://drift"),
        2,
        "the reconnect re-sent the screen-off"
    );
}

/// While the link is up the daemon sees every change, so a panel woken by
/// a person (the TV's own remote) is the new state, not a drift to undo.
#[tokio::test]
async fn a_screen_woken_while_connected_is_left_alone() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://remote").await;
    let t0 = Instant::now();
    client.tick(t0);

    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(31)),
    });
    collect_until(&mut effects, |e| power_state_is(e, "screenoff")).await;
    settle_verify(&client, t0 + Duration::from_secs(1)).await;
    handle.inject_power_state("mock://remote", PowerState::Active);
    collect_until(&mut effects, |e| power_state_is(e, "active")).await;
    for k in 1..=2 {
        client.tick(t0 + PAST_THE_CAP * k);
    }
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        power_state_is(e, "screenoff")
    })
    .await;
    assert_eq!(handle.screen_off_call_count("mock://remote"), 1);
}
