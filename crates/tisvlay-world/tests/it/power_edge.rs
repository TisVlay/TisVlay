//! The going-off events: the TV powering off by itself (a live edge from on
//! to off publishes `event/tv-power-off`, while TisVlay's own power-off
//! doesn't) and its remote powering the PC off (`event/remote-power`), and
//! the system profile's sleep for each.

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
    let body = tisvlay_core::SystemProfileConfig {
        name: "Sleep on TV off".into(),
        on_tv_power_off: Some(action),
        ..Default::default()
    };
    assign_system_profile(effects, client, tv, body).await;
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

/// Two TVs powering off together (or one TV paired twice) ask for one
/// sleep: logind refuses a second while the first is under way. Once the
/// PC wakes, the next power-off asks again.
#[tokio::test]
async fn one_sleep_at_a_time() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    for address in ["mock://sleep-a", "mock://sleep-b"] {
        let tv = add_connected_tv(&mut effects, &client, address).await;
        assign_sleep_profile(
            &mut effects,
            &client,
            tv,
            tisvlay_core::SystemSleepAction::Suspend,
        )
        .await;
    }

    mock.inject_power_state("mock://sleep-a", PowerState::Standby);
    mock.inject_power_state("mock://sleep-b", PowerState::Standby);
    tokio::time::timeout(Duration::from_secs(5), sleeps.recv())
        .await
        .expect("a sleep request")
        .expect("reactor alive");
    let second = tokio::time::timeout(Duration::from_millis(300), sleeps.recv()).await;
    assert!(second.is_err(), "one sleep, not two: {second:?}");

    client.send(tisvlay_world::InEvent::System {
        event: tisvlay_world::SystemEvent::Resume,
        connector: None,
    });
    mock.inject_power_state("mock://sleep-a", PowerState::Active);
    mock.inject_power_state("mock://sleep-a", PowerState::Standby);
    tokio::time::timeout(Duration::from_secs(5), sleeps.recv())
        .await
        .expect("a sleep request after the wake")
        .expect("reactor alive");
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

fn remote_powered(effect: &WireEffect) -> Option<&serde_json::Value> {
    is_publish_on(effect, "/event/remote-power")
}

/// Wait for the next sleep request.
async fn next_sleep(
    sleeps: &mut tokio::sync::mpsc::UnboundedReceiver<tisvlay_world::SleepRequest>,
) -> tisvlay_world::SleepRequest {
    tokio::time::timeout(Duration::from_secs(5), sleeps.recv())
        .await
        .expect("a sleep request")
        .expect("reactor alive")
}

#[tokio::test]
async fn the_remote_power_key_publishes_the_event() {
    let (mut effects, client, mock) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://remote").await;

    mock.press_remote_power("mock://remote");
    let seen = collect_until(&mut effects, |e| remote_powered(e).is_some()).await;
    let payload = seen.iter().find_map(remote_powered).expect("the event");
    assert_eq!(payload["tv"], id.to_string());
}

/// The remote's power key runs the system profile's `on_remote_power`, and
/// only it: the TV's own power-off field stays out of it.
#[tokio::test]
async fn a_remote_power_profile_sleeps_the_pc() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    let tv = add_connected_tv(&mut effects, &client, "mock://remote-sleep").await;
    let body = tisvlay_core::SystemProfileConfig {
        name: "Sleep on the remote".into(),
        on_remote_power: Some(tisvlay_core::SystemSleepAction::Sleep),
        ..Default::default()
    };
    assign_system_profile(&mut effects, &client, tv, body).await;

    mock.press_remote_power("mock://remote-sleep");
    assert_eq!(
        next_sleep(&mut sleeps).await,
        tisvlay_world::SleepRequest {
            tv,
            mode: tisvlay_world::SleepMode::Sleep
        }
    );
    mock.inject_power_state("mock://remote-sleep", PowerState::Standby);
    collect_until(&mut effects, |e| powered_off(e).is_some()).await;
    assert!(
        sleeps.try_recv().is_err(),
        "on_tv_power_off is unset, so the TV powering off sleeps nothing"
    );
}

/// The press is explicit, so it acts with automation gated off, where the
/// TV powering off by itself doesn't.
#[tokio::test]
async fn the_remote_power_key_ignores_the_automation_gate() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    let tv = add_connected_tv(&mut effects, &client, "mock://remote-gated").await;
    // The mock sits on HDMI_1; automating only on the primary, HDMI_2,
    // gates automation off.
    client.command(Command::SetPrimaryInput {
        id: tv,
        input: Some("HDMI_2".into()),
        reply: Some(tisvlay_world::Reply::new(80)),
    });
    collect_until(&mut effects, |e| reply_to(e, 80)).await;
    let body = tisvlay_core::SystemProfileConfig {
        name: "Gated".into(),
        automate_when: Some(tisvlay_core::AutomateWhen::OnPrimary),
        on_tv_power_off: Some(tisvlay_core::SystemSleepAction::Suspend),
        on_remote_power: Some(tisvlay_core::SystemSleepAction::Suspend),
        ..Default::default()
    };
    assign_system_profile(&mut effects, &client, tv, body).await;

    mock.inject_power_state("mock://remote-gated", PowerState::Standby);
    collect_until(&mut effects, |e| powered_off(e).is_some()).await;
    assert!(
        sleeps.try_recv().is_err(),
        "automation gates the TV powering off"
    );
    mock.press_remote_power("mock://remote-gated");
    assert_eq!(
        next_sleep(&mut sleeps).await.mode,
        tisvlay_world::SleepMode::Suspend
    );
}

/// An unmanaged TV's power key still publishes its event, but sleeps
/// nothing: the daemon acts for managed TVs only.
#[tokio::test]
async fn an_unmanaged_tvs_remote_power_key_sleeps_nothing() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    let tv = add_connected_tv(&mut effects, &client, "mock://remote-unmanaged").await;
    let body = tisvlay_core::SystemProfileConfig {
        name: "Suspend on the remote".into(),
        on_remote_power: Some(tisvlay_core::SystemSleepAction::Suspend),
        ..Default::default()
    };
    assign_system_profile(&mut effects, &client, tv, body).await;
    set_managed(&mut effects, &client, tv, false).await;

    mock.press_remote_power("mock://remote-unmanaged");
    collect_until(&mut effects, |e| remote_powered(e).is_some()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(sleeps.try_recv().is_err(), "an unmanaged TV sleeps nothing");
}

#[tokio::test]
async fn a_stream_holds_the_remote_power_sleep_back() {
    let (mut effects, mut sleeps, client, mock) = spawn_reactor_with_sleeper();
    let tv = add_connected_tv(&mut effects, &client, "mock://remote-stream").await;
    let body = tisvlay_core::SystemProfileConfig {
        name: "Sleep on the remote".into(),
        on_remote_power: Some(tisvlay_core::SystemSleepAction::Suspend),
        ..Default::default()
    };
    assign_system_profile(&mut effects, &client, tv, body).await;
    client.send(tisvlay_world::InEvent::Stream { armed: vec![tv] });

    mock.press_remote_power("mock://remote-stream");
    collect_until(&mut effects, |e| remote_powered(e).is_some()).await;
    assert!(
        sleeps.try_recv().is_err(),
        "a stream must hold the sleep back"
    );
}
