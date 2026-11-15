//! A network TV's link dropping while the TV stays on: a router reboot or a
//! Wi-Fi outage, not a standby. The daemon redials such a TV until the
//! network is back, and holds nothing for it meanwhile: a screen-off or a
//! power-off sent to it is refused, and whoever asked decides again once
//! the link returns. A drop the TV announced (a standby report first)
//! stays down.

use crate::support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::PowerState;
use tisvlay_protocol::error::ErrorCode;
use tisvlay_world::{Command, InEvent, Reply};

/// Past the redial backoff's 60 s cap, so each tick lets exactly one more
/// dial through whatever the attempt count.
const PAST_THE_CAP: Duration = Duration::from_secs(61);

type Effects = tokio::sync::mpsc::UnboundedReceiver<tisvlay_world::WireEffect>;

/// Cut the link of a TV that stays on, with the network down behind it,
/// and wait for the world to see the drop.
async fn outage(effects: &mut Effects, handle: &tisvlay_mock::MockHandle, address: &str) {
    handle.set_unreachable(address, true);
    handle.cut_link(address);
    collect_until(effects, |e| link_connected(e, false)).await;
}

/// Advance world time past the backoff cap `k` times, each tick letting
/// one failed dial through, and check the dials came one per tick. The
/// settle before a tick lets the last failure set its backoff first.
async fn ticks_dial_once_each(
    effects: &mut Effects,
    client: &tisvlay_world::WorldClient,
    handle: &tisvlay_mock::MockHandle,
    address: &str,
    t0: Instant,
    k: u32,
) {
    let before = handle.connect_call_count(address);
    for n in 1..=k {
        settle(effects, client, 900 + u64::from(n)).await;
        client.tick(t0 + PAST_THE_CAP * n);
        wait_for_dials(handle, address, before + n as usize).await;
    }
    settle(effects, client, 999).await;
    assert_eq!(
        handle.connect_call_count(address),
        before + k as usize,
        "one dial per backoff period"
    );
}

/// The first redial follows the drop at once; the rest keep coming past
/// the three-attempt budget, a backoff period apart, until one lands.
#[tokio::test]
#[ignore = "a network TV that drops while awake is never redialed"]
async fn a_drop_while_awake_redials_until_the_network_is_back() {
    let (mut effects, client, handle) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://outage").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://outage").await;
    wait_for_dials(&handle, "mock://outage", 2).await;
    ticks_dial_once_each(&mut effects, &client, &handle, "mock://outage", t0, 5).await;

    handle.set_unreachable("mock://outage", false);
    client.tick(t0 + PAST_THE_CAP * 6);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
}

/// An outage is often first noticed by a command: the screen-off finds
/// the link dead. That the daemon was sending a screen-off doesn't make
/// the drop an announced one.
#[tokio::test]
#[ignore = "a network TV that drops while awake is never redialed"]
async fn a_drop_noticed_by_a_screen_off_redials() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://send").await;
    handle.set_unreachable("mock://send", true);
    handle.set_screen_off_closes("mock://send", true);

    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(31)),
    });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    wait_for_dials(&handle, "mock://send", 2).await;
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

/// A power-off can't reach the TV, so it is refused, and the TV, still
/// on as far as anyone knows, keeps being redialed.
#[tokio::test]
#[ignore = "a network TV that drops while awake is never redialed"]
async fn a_power_off_while_unreachable_is_refused_and_the_redial_goes_on() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://off").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://off").await;
    wait_for_dials(&handle, "mock://off", 2).await;

    client.command(Command::PowerOff {
        id,
        reply: Some(Reply::new(51)),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 51)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 51)),
        Some(ErrorCode::NotConnected)
    );
    ticks_dial_once_each(&mut effects, &client, &handle, "mock://off", t0, 2).await;
}

/// A screen-off to a TV last seen awake and now unreachable isn't
/// "already dark": it is refused, and nothing is kept to send later.
#[tokio::test]
#[ignore = "a screen-off to an unreachable TV last seen awake is acked as done"]
async fn a_screen_off_while_unreachable_is_refused_not_held() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://refused").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://refused").await;
    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(41)),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 41)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 41)),
        Some(ErrorCode::NotConnected)
    );

    handle.set_unreachable("mock://refused", false);
    client.tick(t0 + PAST_THE_CAP);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    settle(&mut effects, &client, 42).await;
    assert_eq!(
        handle.screen_off_call_count("mock://refused"),
        0,
        "the reconnect sends nothing the daemon wasn't asked for again"
    );
}

/// A wake during the outage (a screen-on rides a wake dial) spends its
/// own budget and no more: the redial is still there afterwards.
#[tokio::test]
#[ignore = "a network TV that drops while awake is never redialed"]
async fn a_wake_during_the_outage_keeps_the_redial() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://woken").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://woken").await;
    wait_for_dials(&handle, "mock://woken", 2).await;
    client.command(Command::SetScreen {
        id,
        on: true,
        restore_input: false,
        reply: Some(Reply::new(61)),
    });
    collect_until(&mut effects, |e| reply_to(e, 61)).await;

    // Well past the wake's three dials: one more per backoff period.
    for k in 1..=8 {
        settle(&mut effects, &client, 600 + u64::from(k)).await;
        client.tick(t0 + PAST_THE_CAP * k);
    }
    wait_for_dials(&handle, "mock://woken", 8).await;

    handle.set_unreachable("mock://woken", false);
    settle(&mut effects, &client, 62).await;
    client.tick(t0 + PAST_THE_CAP * 9);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
}

/// `reconnect` during the redial doesn't wait out the backoff.
#[tokio::test]
#[ignore = "a network TV that drops while awake is never redialed"]
async fn a_reconnect_during_the_redial_dials_at_once() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://asked").await;
    client.tick(Instant::now());

    outage(&mut effects, &handle, "mock://asked").await;
    wait_for_dials(&handle, "mock://asked", 2).await;
    settle(&mut effects, &client, 70).await;

    handle.set_unreachable("mock://asked", false);
    client.command(Command::Reconnect {
        id,
        reply: Some(Reply::new(71)),
    });
    collect_until(&mut effects, |e| link_connected(e, true)).await;
}

/// The same outage seen from the PC's side: its own link drops, the TV is
/// parked, and the link is back before the path to the TV is. The redial
/// that follows doesn't give up after three dials.
#[tokio::test]
#[ignore = "an awake TV parked by a carrier loss gets three dials when the carrier returns"]
async fn an_awake_tv_parked_by_a_carrier_loss_redials_past_the_budget() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://parked").await;
    let t0 = Instant::now();
    client.tick(t0);
    client.send(InEvent::RouteResolved {
        tv: id,
        interface: Some("eth0".into()),
    });
    settle(&mut effects, &client, 80).await;

    handle.set_unreachable("mock://parked", true);
    client.send(InEvent::Carrier {
        interface: "eth0".into(),
        up: false,
    });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    client.send(InEvent::Carrier {
        interface: "eth0".into(),
        up: true,
    });
    wait_for_dials(&handle, "mock://parked", 2).await;
    ticks_dial_once_each(&mut effects, &client, &handle, "mock://parked", t0, 5).await;
}
