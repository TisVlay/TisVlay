//! The wake list: every TV's device node with its system profile's
//! `on_tv_power_on`, sent for the daemon to write whenever it changes.

use crate::support;

use std::time::Duration;

use support::*;
use tisvlay_core::SystemWakeAction;
use tisvlay_world::{Command, Reply, WakeList};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

/// Wait for a sent list equal to `want`; returns every list sent before it.
async fn wait_list(
    lists: &mut UnboundedReceiver<WakeList>,
    want: &[(&str, SystemWakeAction)],
) -> Vec<WakeList> {
    let want: WakeList = want
        .iter()
        .map(|(node, action)| (node.into(), *action))
        .collect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut before = Vec::new();
    loop {
        let list = tokio::time::timeout_at(deadline, lists.recv())
            .await
            .unwrap_or_else(|_| panic!("the wake list never became {want:?}: {before:?}"))
            .expect("reactor alive");
        if list == want {
            return before;
        }
        before.push(list);
    }
}

fn wake_profile(action: SystemWakeAction) -> tisvlay_core::SystemProfileConfig {
    tisvlay_core::SystemProfileConfig {
        name: format!("Wake {action:?}"),
        on_tv_power_on: Some(action),
        ..Default::default()
    }
}

/// A TV whose connection needs `node`.
async fn add_local_tv(
    effects: &mut UnboundedReceiver<tisvlay_world::WireEffect>,
    client: &tisvlay_world::WorldClient,
    address: &str,
    node: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let mut config = mock_config(address);
    config.insert("local_device", node);
    client.command(Command::AddTv {
        record: Box::new(tv_record(id, "Den", config)),
        reply: Some(Reply::new(1)),
    });
    collect_until(effects, |e| link_connected(e, true)).await;
    id
}

/// The first step sends the empty list, replacing one a previous daemon
/// left; a TV with a node lands on it with the default `none`, and its
/// profile switches it to `wake` and back.
#[tokio::test]
async fn the_profile_sets_the_nodes_action() {
    let (mut effects, mut lists, client, _mock) = spawn_reactor_with_wake_list();
    let tv = add_local_tv(&mut effects, &client, "mock://wake", "/dev/cec-wake").await;
    let before = wait_list(&mut lists, &[("/dev/cec-wake", SystemWakeAction::None)]).await;
    assert_eq!(before, [WakeList::new()], "the first step's empty list");

    assign_system_profile(
        &mut effects,
        &client,
        tv,
        wake_profile(SystemWakeAction::Wake),
    )
    .await;
    wait_list(&mut lists, &[("/dev/cec-wake", SystemWakeAction::Wake)]).await;
    assign_system_profile(
        &mut effects,
        &client,
        tv,
        wake_profile(SystemWakeAction::None),
    )
    .await;
    wait_list(&mut lists, &[("/dev/cec-wake", SystemWakeAction::None)]).await;
}

/// A TV without a local device node (a network TV) never lands on the
/// list; one node shared by two TVs wakes the PC if either asks to.
#[tokio::test]
async fn only_tvs_with_a_node_are_listed_and_a_wake_wins() {
    let (mut effects, mut lists, client, _mock) = spawn_reactor_with_wake_list();
    let network = add_connected_tv(&mut effects, &client, "mock://wake-net").await;
    assign_system_profile(
        &mut effects,
        &client,
        network,
        wake_profile(SystemWakeAction::Wake),
    )
    .await;
    let local = add_local_tv(&mut effects, &client, "mock://wake-a", "/dev/cec-a").await;
    assign_system_profile(
        &mut effects,
        &client,
        local,
        wake_profile(SystemWakeAction::None),
    )
    .await;
    let waking = add_local_tv(&mut effects, &client, "mock://wake-b", "/dev/cec-b").await;
    assign_system_profile(
        &mut effects,
        &client,
        waking,
        wake_profile(SystemWakeAction::Wake),
    )
    .await;
    let shared = add_local_tv(&mut effects, &client, "mock://wake-b2", "/dev/cec-b").await;
    assign_system_profile(
        &mut effects,
        &client,
        shared,
        wake_profile(SystemWakeAction::None),
    )
    .await;

    let want = [
        ("/dev/cec-a", SystemWakeAction::None),
        ("/dev/cec-b", SystemWakeAction::Wake),
    ];
    wait_list(&mut lists, &want).await;
    let list = tokio::time::timeout(Duration::from_millis(300), lists.recv()).await;
    assert!(list.is_err(), "the list changed again: {list:?}");
}
