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
