//! The TV powering off by itself: a live edge from on to off publishes
//! `event/tv-power-off`, while TisVlay's own power-off doesn't.

use crate::support;

use std::time::Duration;

use support::*;
use tisvlay_core::PowerState;
use tisvlay_world::{Command, WireEffect};

fn powered_off(effect: &WireEffect) -> Option<&serde_json::Value> {
    is_publish_on(effect, "/event/tv-power-off")
}

#[tokio::test]
async fn the_tv_powering_off_by_itself_publishes_the_event() {
    let (mut effects, client, mock) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://edge").await;

    mock.inject_power_state("mock://edge", PowerState::Standby);
    let seen = collect_until(&mut effects, |e| powered_off(e).is_some()).await;
    let payload = seen.iter().find_map(powered_off).expect("the event");
    assert_eq!(payload["tv"], id.to_string());
}

#[tokio::test]
async fn tisvlays_own_power_off_publishes_nothing() {
    let (mut effects, client, _mock) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://own").await;

    client.command(Command::PowerOff { id, reply: None });
    // The event would publish in the standby report's own step, ahead of
    // the state it derives from.
    let seen = collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "standby")
    })
    .await;
    assert!(
        !seen.iter().any(|e| powered_off(e).is_some()),
        "TisVlay's own power-off published the event: {seen:#?}"
    );
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        powered_off(e).is_some()
    })
    .await;
}

/// Give `tv` a system profile whose `on_tv_power_off` is `action`.
async fn assign_sleep_profile(
    effects: &mut tokio::sync::mpsc::UnboundedReceiver<WireEffect>,
    client: &tisvlay_world::WorldClient,
    tv: uuid::Uuid,
    action: tisvlay_core::SystemSleepAction,
) {
    let profile = uuid::Uuid::new_v4();
    client.command(Command::AddProfile {
        record: Box::new(tisvlay_world::ProfileRecord {
            id: profile,
            body: tisvlay_core::SystemProfileConfig {
                name: "Sleep on TV off".into(),
                on_tv_power_off: Some(action),
                ..Default::default()
            },
        }),
        reply: Some(tisvlay_world::Reply::new(90)),
    });
    collect_until(effects, |e| reply_to(e, 90)).await;
    client.command(Command::SetSystemProfile {
        id: tv,
        profile: Some(profile),
        reply: Some(tisvlay_world::Reply::new(91)),
    });
    collect_until(effects, |e| reply_to(e, 91)).await;
}

#[tokio::test]
async fn a_system_profile_sleep_asks_the_daemon() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    let tv = add_connected_tv(&mut effects, &client, "mock://sleep").await;
    assign_sleep_profile(
        &mut effects,
        &client,
        tv,
        tisvlay_core::SystemSleepAction::Suspend,
    )
    .await;

    mock.inject_power_state("mock://sleep", PowerState::Standby);
    let request = tokio::time::timeout(Duration::from_secs(5), sleeps.recv())
        .await
        .expect("a sleep request")
        .expect("reactor alive");
    assert_eq!(
        request,
        tisvlay_world::SleepRequest {
            tv,
            mode: tisvlay_world::SleepMode::Suspend
        }
    );
}

#[tokio::test]
async fn a_stream_holds_the_sleep_back() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    let tv = add_connected_tv(&mut effects, &client, "mock://sleep-stream").await;
    assign_sleep_profile(
        &mut effects,
        &client,
        tv,
        tisvlay_core::SystemSleepAction::Sleep,
    )
    .await;
    client.send(tisvlay_world::InEvent::Stream { armed: vec![tv] });

    mock.inject_power_state("mock://sleep-stream", PowerState::Standby);
    collect_until(&mut effects, |e| powered_off(e).is_some()).await;
    assert!(
        sleeps.try_recv().is_err(),
        "a stream must hold the sleep back"
    );
}

#[tokio::test]
async fn the_stock_profile_sleeps_nothing() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    add_connected_tv(&mut effects, &client, "mock://sleep-stock").await;

    mock.inject_power_state("mock://sleep-stock", PowerState::Standby);
    collect_until(&mut effects, |e| powered_off(e).is_some()).await;
    assert!(
        sleeps.try_recv().is_err(),
        "the stock profile asks no sleep"
    );
}

#[tokio::test]
async fn a_refused_sleep_publishes_dispatch_failed() {
    let (mut effects, client, _mock) = spawn_reactor();
    let tv = uuid::Uuid::new_v4();
    client.send(tisvlay_world::InEvent::SleepRefused {
        tv,
        mode: tisvlay_world::SleepMode::Suspend,
        message: "polkit said no".into(),
    });
    let seen = collect_until(&mut effects, |e| {
        is_publish_on(e, "/event/dispatch-failed").is_some()
    })
    .await;
    let payload = seen
        .iter()
        .find_map(|e| is_publish_on(e, "/event/dispatch-failed"))
        .expect("the failure");
    assert_eq!(payload["tv"], tv.to_string());
    assert_eq!(payload["action"], "suspend");
}
