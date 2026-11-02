//! Held wakes: a wake waiting on a missing route or device expires into
//! a plain reconnect, and a newer off intent cancels it, for network and
//! local-device TVs alike. A plain reconnect never powers the TV on.

use crate::support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::VendorConfig;
use tisvlay_world::{Command, InEvent, Reply, SystemEvent, WireEffect, WorldClient};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

fn failed(effect: &WireEffect) -> bool {
    is_publish_on(effect, "event/connect-failed").is_some()
}

fn power_on(client: &WorldClient, id: Uuid, reply: u64) {
    client.command(Command::PowerOn {
        id,
        restore_input: false,
        reply: Some(Reply::new(reply)),
    });
}

/// A network TV whose first dial failed and whose route is now known
/// gone, so a wake will be held.
async fn routeless_tv(
    effects: &mut UnboundedReceiver<WireEffect>,
    client: &WorldClient,
    address: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut config = mock_config(address);
    config.insert("refuse_connect", true);
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Den", config)),
        reply: None,
    });
    collect_until(effects, failed).await;
    client.send(InEvent::RouteResolved {
        tv: id,
        interface: None,
    });
    id
}

/// A local-device TV whose device is reported gone, so a wake will be
/// held.
async fn deviceless_tv(
    effects: &mut UnboundedReceiver<WireEffect>,
    requests: &mut UnboundedReceiver<tisvlay_world::DeviceRequest>,
    client: &WorldClient,
    address: &str,
    emulate_screen: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut config: VendorConfig = mock_config(address);
    config.insert("local_device", "/dev/cec-held");
    config.insert("emulate_screen_via_power", emulate_screen);
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Den", config)),
        reply: None,
    });
    let _monitor = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("a monitor request within 5 s");
    client.send(InEvent::DeviceAvailability {
        tv: id,
        available: false,
    });
    settle(effects, client, 900).await;
    id
}

/// A wake held on a missing route expires after the limit: it reports
/// failed, and the route's later return only reconnects.
#[tokio::test]
async fn a_routeless_wake_expires_into_a_plain_reconnect() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = routeless_tv(&mut effects, &client, "mock://expire").await;
    let t0 = Instant::now();
    client.tick(t0);
    power_on(&client, id, 1);
    collect_until(&mut effects, |e| phase_published(e, "awaiting_network")).await;

    // Within the limit the wake is still held.
    client.tick(t0 + Duration::from_secs(30));
    assert_quiet(&mut effects, Duration::from_millis(200), |e| {
        phase_published(e, "failed")
    })
    .await;
    client.tick(t0 + Duration::from_secs(61));
    collect_until(&mut effects, |e| phase_published(e, "failed")).await;

    client.tick(t0 + Duration::from_secs(70));
    client.send(InEvent::RouteResolved {
        tv: id,
        interface: Some("eth0".into()),
    });
    let seen = collect_until(&mut effects, failed).await;
    assert!(
        !seen.iter().any(|e| phase_published(e, "wol_sent")),
        "the expired wake's return dials without waking: {seen:#?}"
    );
    assert_eq!(handle.connect_call_count("mock://expire"), 2);
}

/// A wake held on a missing route is cancelled by a later power-off: the
/// rpc succeeds, the wake reports failed, and the route's return only
/// reconnects.
#[tokio::test]
async fn a_power_off_cancels_a_routeless_wake() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = routeless_tv(&mut effects, &client, "mock://cancel").await;
    power_on(&client, id, 1);
    collect_until(&mut effects, |e| phase_published(e, "awaiting_network")).await;

    client.command(Command::PowerOff {
        id,
        reply: Some(Reply::new(2)),
    });
    // The reply and the cancelled wake's report go out in the same step.
    let seen = collect_until(&mut effects, |e| phase_published(e, "failed")).await;
    assert!(
        seen.iter()
            .any(|e| reply_to(e, 2) && reply_err_code(e, 2).is_none()),
        "cancelling the held wake is the power-off: {seen:#?}"
    );

    client.send(InEvent::RouteResolved {
        tv: id,
        interface: Some("eth0".into()),
    });
    let seen = collect_until(&mut effects, failed).await;
    assert!(
        !seen.iter().any(|e| phase_published(e, "wol_sent")),
        "{seen:#?}"
    );
}

/// The profile's sleep action (power off) cancels a held wake too, so a
/// boot wake held across a lock and idle can't power the TV on later.
#[tokio::test]
async fn a_profile_power_off_cancels_a_held_wake() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = routeless_tv(&mut effects, &client, "mock://sleep").await;
    client.send(InEvent::System {
        event: SystemEvent::Startup,
        connector: None,
    });
    collect_until(&mut effects, |e| phase_published(e, "awaiting_network")).await;
    client.send(InEvent::System {
        event: SystemEvent::Sleep,
        connector: None,
    });
    collect_until(&mut effects, |e| phase_published(e, "failed")).await;
    client.send(InEvent::RouteResolved {
        tv: id,
        interface: Some("eth0".into()),
    });
    let seen = collect_until(&mut effects, failed).await;
    assert!(
        !seen.iter().any(|e| phase_published(e, "wol_sent")),
        "{seen:#?}"
    );
}

/// Without a power-off, the route's return within the limit still wakes:
/// the hold is for boot and resume.
#[tokio::test]
async fn a_wake_still_goes_out_when_the_route_returns_in_time() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = routeless_tv(&mut effects, &client, "mock://intime").await;
    let t0 = Instant::now();
    client.tick(t0);
    power_on(&client, id, 1);
    collect_until(&mut effects, |e| phase_published(e, "awaiting_network")).await;
    client.tick(t0 + Duration::from_secs(20));
    client.send(InEvent::RouteResolved {
        tv: id,
        interface: Some("eth0".into()),
    });
    collect_until(&mut effects, |e| phase_published(e, "wol_sent")).await;
}

/// A wake held on a missing device expires the same way, and the device's
/// later return only reconnects.
#[tokio::test]
async fn a_deviceless_wake_expires_into_a_plain_reconnect() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = deviceless_tv(
        &mut effects,
        &mut requests,
        &client,
        "mock://dev-expire",
        false,
    )
    .await;
    let t0 = Instant::now();
    client.tick(t0);
    power_on(&client, id, 1);
    collect_until(&mut effects, |e| phase_published(e, "awaiting_device")).await;
    client.tick(t0 + Duration::from_secs(61));
    collect_until(&mut effects, |e| phase_published(e, "failed")).await;

    client.send(InEvent::DeviceAvailability {
        tv: id,
        available: true,
    });
    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(
        !seen.iter().any(|e| phase_published(e, "wol_sent")),
        "{seen:#?}"
    );
    assert_eq!(handle.connect_call_count("mock://dev-expire"), 1);
}

/// On a backend that emulates the screen as power, a screen-off is a
/// power-off: it cancels a wake held on a missing device.
#[tokio::test]
async fn an_emulated_screen_off_cancels_a_deviceless_wake() {
    let (mut effects, mut requests, client, _handle) = spawn_reactor_with_devices();
    let id = deviceless_tv(
        &mut effects,
        &mut requests,
        &client,
        "mock://dev-screen",
        true,
    )
    .await;
    power_on(&client, id, 1);
    collect_until(&mut effects, |e| reply_to(e, 1)).await;
    client.command(Command::SetScreen {
        id,
        on: false,
        restore_input: false,
        reply: Some(Reply::new(2)),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 2)).await;
    assert!(
        seen.iter().any(|e| phase_published(e, "failed")),
        "{seen:#?}"
    );
    client.send(InEvent::DeviceAvailability {
        tv: id,
        available: true,
    });
    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(
        !seen.iter().any(|e| phase_published(e, "wol_sent")),
        "{seen:#?}"
    );
}
