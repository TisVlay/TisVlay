//! The wake list: the device nodes of the TVs whose system profile's
//! `on_tv_power_on` is `wake`, sent for the daemon to write whenever the set
//! changes.

use crate::support;

use std::path::PathBuf;
use std::time::Duration;

use support::*;
use tisvlay_world::{Command, Reply};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

/// Wait for a sent list equal to `want`; returns every list sent before it.
async fn wait_list(
    lists: &mut UnboundedReceiver<Vec<PathBuf>>,
    want: &[&str],
) -> Vec<Vec<PathBuf>> {
    let want: Vec<PathBuf> = want.iter().map(PathBuf::from).collect();
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

fn wake_profile(wake: bool) -> tisvlay_core::SystemProfileConfig {
    tisvlay_core::SystemProfileConfig {
        name: format!("Wake {wake}"),
        on_tv_power_on: Some(if wake {
            tisvlay_core::SystemWakeAction::Wake
        } else {
            tisvlay_core::SystemWakeAction::None
        }),
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
/// left; a profile asking for the wake adds the TV's node, and taking it
/// back removes it.
#[tokio::test]
async fn the_profile_puts_the_node_on_the_list() {
    let (mut effects, mut lists, client, _mock) = spawn_reactor_with_wake_list();
    let tv = add_local_tv(&mut effects, &client, "mock://wake", "/dev/cec-wake").await;
    wait_list(&mut lists, &[]).await;

    assign_system_profile(&mut effects, &client, tv, wake_profile(true)).await;
    wait_list(&mut lists, &["/dev/cec-wake"]).await;
    assign_system_profile(&mut effects, &client, tv, wake_profile(false)).await;
    wait_list(&mut lists, &[]).await;
}

/// A TV without a local device node (a network TV) never lands on the
/// list, and one asking for nothing doesn't either.
#[tokio::test]
async fn only_waking_tvs_with_a_node_are_listed() {
    let (mut effects, mut lists, client, _mock) = spawn_reactor_with_wake_list();
    let network = add_connected_tv(&mut effects, &client, "mock://wake-net").await;
    assign_system_profile(&mut effects, &client, network, wake_profile(true)).await;
    let local = add_local_tv(&mut effects, &client, "mock://wake-local", "/dev/cec-a").await;
    assign_system_profile(&mut effects, &client, local, wake_profile(false)).await;
    let waking = add_local_tv(&mut effects, &client, "mock://wake-b", "/dev/cec-b").await;
    assign_system_profile(&mut effects, &client, waking, wake_profile(true)).await;

    let before = wait_list(&mut lists, &["/dev/cec-b"]).await;
    assert!(
        before.iter().all(Vec::is_empty),
        "nothing else was ever listed: {before:?}"
    );
}
