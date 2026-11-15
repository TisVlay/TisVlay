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
/// settle before a tick lets the last failure set its backoff first; its
/// barrier ids (900 and up) stay clear of the tests' own reply ids.
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
/// own budget and no more: the redial is still there afterwards. The
/// wake's dial doesn't turn the TV into one requests are held for: a
/// screen-off behind it is refused, and the return sends neither.
#[tokio::test]
#[ignore = "a screen-off to a TV being redialed is held, not refused"]
async fn a_wake_during_the_outage_keeps_the_redial_and_holds_nothing() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://woken").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://woken").await;
    wait_for_dials(&handle, "mock://woken", 2).await;
    client.command(Command::SetScreen {
        id,
        on: true,
        restore_input: true,
        reply: Some(Reply::new(61)),
    });
    collect_until(&mut effects, |e| reply_to(e, 61)).await;
    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(62)),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 62)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 62)),
        Some(ErrorCode::NotConnected)
    );

    // Well past the wake's three dials: still one more per backoff period.
    wait_for_dials(&handle, "mock://woken", 3).await;
    ticks_dial_once_each(&mut effects, &client, &handle, "mock://woken", t0, 6).await;

    handle.set_unreachable("mock://woken", false);
    client.tick(t0 + PAST_THE_CAP * 7);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    settle(&mut effects, &client, 63).await;
    assert_eq!(handle.screen_on_call_count("mock://woken"), 0);
    assert_eq!(handle.screen_off_call_count("mock://woken"), 0);
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        is_publish_on(e, "/state/input").is_some()
    })
    .await;
}

/// `reconnect` during the redial doesn't wait out the backoff.
#[tokio::test]
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

/// The streak's pace: the first three failures wait the 3 s backoff, as
/// a budgeted streak does, and from the fourth the wait doubles.
#[tokio::test]
async fn the_redial_backs_off_past_the_budget() {
    let (mut effects, client, handle) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://pace").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://pace").await;
    wait_for_dials(&handle, "mock://pace", 2).await;
    for (dials, at) in [(3, 4), (4, 8), (5, 12)] {
        settle(&mut effects, &client, 100 + at).await;
        client.tick(t0 + Duration::from_secs(at));
        wait_for_dials(&handle, "mock://pace", dials).await;
    }
    settle(&mut effects, &client, 120).await;
    client.tick(t0 + Duration::from_secs(16));
    settle(&mut effects, &client, 121).await;
    assert_eq!(
        handle.connect_call_count("mock://pace"),
        5,
        "the fourth failure waits 6 s"
    );
    client.tick(t0 + Duration::from_secs(19));
    wait_for_dials(&handle, "mock://pace", 6).await;
}

/// A power-off the daemon sent closes the link from its own side. Even
/// when the TV never reports the standby, that drop is announced.
#[tokio::test]
async fn a_power_off_without_a_standby_report_stays_down() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://quiet-off").await;
    let t0 = Instant::now();
    client.tick(t0);
    handle.set_power_off_unreported("mock://quiet-off", true);

    client.command(Command::PowerOff { id, reply: None });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    for k in 1..=3 {
        client.tick(t0 + PAST_THE_CAP * k);
    }
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, true)
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://quiet-off"), 1);
}

/// A daemon started during the outage has no last report to judge by. A
/// managed network TV that spends its budget keeps being dialed, and
/// meanwhile it is unreachable like any other: a screen-off is refused,
/// not acked as already dark.
#[tokio::test]
#[ignore = "a screen-off to a TV being redialed is acked as done"]
async fn a_tv_unreachable_since_the_start_keeps_being_dialed() {
    let (mut effects, client, handle) = spawn_reactor();
    let t0 = Instant::now();
    client.tick(t0);
    handle.set_unreachable("mock://boot", true);
    let id = uuid::Uuid::new_v4();
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Boot", mock_config("mock://boot"))),
        reply: Some(Reply::new(1)),
    });
    wait_for_dials(&handle, "mock://boot", 1).await;
    ticks_dial_once_each(&mut effects, &client, &handle, "mock://boot", t0, 5).await;

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

    handle.set_unreachable("mock://boot", false);
    client.tick(t0 + PAST_THE_CAP * 6);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
}

/// The journal tells the two kinds of drop apart, and says when an
/// outage ended.
#[tracing_test::traced_test]
#[tokio::test]
async fn the_log_says_which_drop_it_was_and_when_the_link_is_back() {
    let (mut effects, client, handle) = spawn_reactor_in_span();
    add_connected_tv(&mut effects, &client, "mock://logged").await;
    let t0 = Instant::now();
    client.tick(t0);

    outage(&mut effects, &handle, "mock://logged").await;
    wait_for_dials(&handle, "mock://logged", 2).await;
    assert!(logs_contain("dropped with no standby announced, redialing"));
    handle.set_unreachable("mock://logged", false);
    settle(&mut effects, &client, 10).await;
    client.tick(t0 + PAST_THE_CAP);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(logs_contain("link back after an outage"));

    handle.inject_power_state("mock://logged", PowerState::ActiveStandby);
    collect_until(&mut effects, |e| power_state_is(e, "activestandby")).await;
    handle.cut_link("mock://logged");
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    assert!(logs_contain("closed with the TV going off; staying down"));
}
