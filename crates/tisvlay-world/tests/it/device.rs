//! Local-device availability: the device watcher's reports gate, park and
//! redial TVs whose connection needs a device node (CEC's `/dev/cecN`).

use crate::support;

use std::time::{Duration, Instant};

use support::*;
use tisvlay_core::VendorConfig;
use tisvlay_world::{BackendEvent, Command, DeviceRequest, InEvent, Reply, WireEffect};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

const NODE: &str = "/dev/cec-test";

/// A mock TV whose connection needs [`NODE`].
fn local_config(address: &str) -> VendorConfig {
    let mut config = mock_config(address);
    config.insert("local_device", NODE);
    config
}

fn add(client: &tisvlay_world::WorldClient, id: Uuid, config: VendorConfig) {
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Den", config)),
        reply: None,
    });
}

fn report(client: &tisvlay_world::WorldClient, tv: Uuid, available: bool) {
    client.send(InEvent::DeviceAvailability { tv, available });
}

fn failed(effect: &WireEffect) -> bool {
    is_publish_on(effect, "event/connect-failed").is_some()
}

/// The watcher is asked to monitor the TV's node.
async fn expect_monitor(requests: &mut UnboundedReceiver<DeviceRequest>, tv: Uuid) {
    let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("a monitor request within 5 s")
        .expect("the request channel stays open");
    assert_eq!(
        request,
        DeviceRequest::Monitor {
            tv,
            path: NODE.into()
        }
    );
}

/// With a watcher attached, a local-device TV's first dial waits for the
/// watcher's first report instead of racing it into a failure.
#[tokio::test]
async fn a_first_dial_waits_for_the_first_report() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    add(&client, id, local_config("mock://first"));
    expect_monitor(&mut requests, id).await;
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, true)
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://first"), 0);

    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.connect_call_count("mock://first"), 1);
}

/// A network TV dials at once: the wait is for local-device TVs only.
#[tokio::test]
async fn a_network_tv_doesnt_wait_for_the_watcher() {
    let (mut effects, mut requests, client, _handle) = spawn_reactor_with_devices();
    add(&client, Uuid::new_v4(), mock_config("mock://network"));
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(
        requests.try_recv().is_err(),
        "a network TV has no device to watch"
    );
}

/// An absent device holds the dial without spending its budget; the
/// device's return dials it.
#[tokio::test]
async fn an_absent_device_holds_the_dial_until_it_returns() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    add(&client, id, local_config("mock://absent"));
    expect_monitor(&mut requests, id).await;
    report(&client, id, false);
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        failed(e) || link_connected(e, true)
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://absent"), 0);

    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.connect_call_count("mock://absent"), 1);
}

/// Removal parks the live connection, and nothing redials while the
/// device is gone; its return redials.
#[tokio::test]
async fn removal_parks_and_return_redials() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    add(&client, id, local_config("mock://park"));
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;

    report(&client, id, false);
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    // Even past the backoff: parked is parked until the device returns.
    client.tick(Instant::now() + Duration::from_secs(10));
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, true)
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://park"), 1);

    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.connect_call_count("mock://park"), 2);
}

/// A dropped local-device connection redials after one backoff by
/// itself: a remove-and-add too quick for the watcher to report must not
/// leave the TV down.
#[tokio::test]
async fn a_dropped_connection_redials_after_one_backoff() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    add(&client, id, local_config("mock://drop"));
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;

    let t0 = Instant::now();
    client.tick(t0);
    // The world's first dial is dial 1.
    client.send(InEvent::Backend {
        tv: id,
        dial: 1,
        event: BackendEvent::Disconnected,
    });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    assert_quiet(&mut effects, Duration::from_millis(200), |e| {
        link_connected(e, true)
    })
    .await;
    client.tick(t0 + Duration::from_secs(4));
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.connect_call_count("mock://drop"), 2);
}

/// The device's return cuts a backed-off retry short: the TV dials at
/// once instead of waiting out the backoff.
#[tokio::test]
async fn the_return_cuts_a_backoff_short() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    let mut config = local_config("mock://backoff");
    config.insert("refuse_connect", true);
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    collect_until(&mut effects, failed).await;
    assert_eq!(handle.connect_call_count("mock://backoff"), 1);

    // Gone and back with no time passing: the retry doesn't wait for its
    // backoff to elapse.
    report(&client, id, false);
    settle(&mut effects, &client, 10).await;
    report(&client, id, true);
    collect_until(&mut effects, failed).await;
    assert_eq!(handle.connect_call_count("mock://backoff"), 2);
}

/// A dial that fails while the device is reported gone was never an
/// attempt: no connect-failed event.
#[tokio::test]
async fn a_failure_with_the_device_gone_isnt_an_attempt() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    handle.set_connect_delay(Duration::from_millis(300));
    let id = Uuid::new_v4();
    let mut config = local_config("mock://vanish");
    config.insert("refuse_connect", true);
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    // The device vanishes while the delayed dial is still in flight.
    tokio::time::sleep(Duration::from_millis(50)).await;
    report(&client, id, false);
    assert_quiet(&mut effects, Duration::from_millis(600), failed).await;
    assert_eq!(handle.connect_call_count("mock://vanish"), 1);
}

/// A re-pair with the device gone dials anyway and fails with a reason,
/// instead of waiting on the device forever.
#[tokio::test]
async fn a_re_pair_dials_through_an_absent_device() {
    let (mut effects, mut requests, client, _handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    let mut config = local_config("mock://repair");
    config.insert("pairing", "accept");
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    client.command(Command::UpdateConfig {
        id,
        patch: serde_json::from_value(serde_json::json!({ "refuse_connect": true })).unwrap(),
        reply: Some(Reply::new(2)),
    });
    collect_until(&mut effects, |e| reply_to(e, 2)).await;
    report(&client, id, false);
    settle(&mut effects, &client, 3).await;

    client.command(Command::RePair {
        id,
        attempt: Uuid::new_v4(),
        reply: Some(Reply::new(4)),
    });
    let progress = format!("tisvlay/test/pairing/{id}/progress");
    collect_until(&mut effects, |e| match e {
        WireEffect::Publish { key, payload } => key == &progress && payload["phase"] == "failed",
        _ => false,
    })
    .await;
}

/// Removing a TV stops its device monitoring.
#[tokio::test]
async fn removal_forgets_the_device() {
    let (mut effects, mut requests, client, _handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    add(&client, id, local_config("mock://forget"));
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    client.command(Command::RemoveTv {
        id,
        reply: Some(Reply::new(5)),
    });
    collect_until(&mut effects, |e| reply_to(e, 5)).await;
    let forgot = std::iter::from_fn(|| requests.try_recv().ok())
        .any(|request| request == DeviceRequest::Forget { tv: id });
    assert!(forgot, "the watcher is told to forget a removed TV");
}

/// Wait until the mock has seen `n` connect calls for `address`.
async fn wait_for_dials(handle: &tisvlay_mock::MockHandle, address: &str, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while handle.connect_call_count(address) < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for dial {n} (saw {})",
            handle.connect_call_count(address)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A device that can't be opened keeps the TV retrying past its budget,
/// on a growing backoff, with the failure event only for the budgeted
/// attempts.
#[tokio::test]
async fn a_device_level_failure_keeps_retrying_past_the_budget() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    let mut config = local_config("mock://busy");
    config.insert("device_unavailable", true);
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    let t0 = Instant::now();
    client.tick(t0);
    report(&client, id, true);
    collect_until(&mut effects, failed).await;
    for (n, at) in [(2, 4), (3, 8)] {
        client.tick(t0 + Duration::from_secs(at));
        collect_until(&mut effects, failed).await;
        assert_eq!(handle.connect_call_count("mock://busy"), n);
    }
    // Past the budget: the 3 s gap still holds once more, then doubles.
    client.tick(t0 + Duration::from_secs(12));
    wait_for_dials(&handle, "mock://busy", 4).await;
    assert_quiet(&mut effects, Duration::from_millis(200), failed).await;
    client.tick(t0 + Duration::from_secs(16));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        handle.connect_call_count("mock://busy"),
        4,
        "the fourth failure waits 6 s"
    );
    client.tick(t0 + Duration::from_secs(19));
    wait_for_dials(&handle, "mock://busy", 5).await;
}

/// A TV that doesn't answer through a working device keeps the budget:
/// three tries, then it waits to be asked.
#[tokio::test]
async fn a_tv_level_failure_keeps_the_budget() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    let mut config = local_config("mock://silent");
    config.insert("refuse_connect", true);
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    let t0 = Instant::now();
    client.tick(t0);
    report(&client, id, true);
    collect_until(&mut effects, failed).await;
    for at in [4, 8] {
        client.tick(t0 + Duration::from_secs(at));
        collect_until(&mut effects, failed).await;
    }
    client.tick(t0 + Duration::from_secs(120));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(handle.connect_call_count("mock://silent"), 3);
}

/// A wake against a device that can't be opened keeps its budget and
/// reports failure, then the TV goes on as a plain reconnect.
#[tokio::test]
async fn a_failed_wake_keeps_its_budget_and_continues_as_a_reconnect() {
    let (mut effects, mut requests, client, handle) = spawn_reactor_with_devices();
    let id = Uuid::new_v4();
    let mut config = local_config("mock://wake");
    config.insert("device_unavailable", true);
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    let t0 = Instant::now();
    client.tick(t0);
    report(&client, id, true);
    collect_until(&mut effects, failed).await;
    client.command(Command::PowerOn {
        id,
        restore_input: false,
        reply: Some(Reply::new(7)),
    });
    collect_until(&mut effects, |e| reply_to(e, 7)).await;
    // The upgrade to a wake starts a fresh budget at once.
    collect_until(&mut effects, failed).await;
    client.tick(t0 + Duration::from_secs(4));
    collect_until(&mut effects, failed).await;
    // The budget's last failure reports the wake failed in the same step,
    // ahead of its connect-failed event: one drain sees both.
    client.tick(t0 + Duration::from_secs(8));
    let seen = collect_until(&mut effects, failed).await;
    assert!(
        seen.iter().any(|e| {
            is_publish_on(e, "progress/power").is_some_and(|p| p["phase"] == "failed")
        }),
        "the spent wake reports failed: {seen:#?}"
    );
    let dials = handle.connect_call_count("mock://wake");
    client.tick(t0 + Duration::from_secs(12));
    wait_for_dials(&handle, "mock://wake", dials + 1).await;
}

/// The device returning while a dial is in flight: if that dial fails,
/// the TV retries at once instead of waiting out the backoff.
#[tokio::test]
async fn a_return_during_a_dial_retries_its_failure_at_once() {
    let (_effects, mut requests, client, handle) = spawn_reactor_with_devices();
    handle.set_connect_delay(Duration::from_millis(300));
    let id = Uuid::new_v4();
    let mut config = local_config("mock://inflight");
    config.insert("device_unavailable", true);
    add(&client, id, config);
    expect_monitor(&mut requests, id).await;
    report(&client, id, true);
    wait_for_dials(&handle, "mock://inflight", 1).await;
    // Gone and back while the first dial is still in flight.
    report(&client, id, false);
    tokio::time::sleep(Duration::from_millis(20)).await;
    report(&client, id, true);
    // No tick: only the prompt retry can dial again.
    wait_for_dials(&handle, "mock://inflight", 2).await;
}
