//! Carrier parking and connector-scoped DPMS dispatch.

mod support;

use std::time::Duration;

use support::*;
use tisvlay_world::{Command, InEvent, SystemEvent};
use uuid::Uuid;

/// Carrier down parks network-dependent connections; carrier up redials.
/// No auto-redial in between — parked is parked.
#[tokio::test]
async fn carrier_transitions_park_and_redial() {
    let (mut effects, client, handle) = spawn_reactor();
    let _id = add_connected_tv(&mut effects, &client, "mock://carrier").await;
    assert_eq!(handle.connect_call_count("mock://carrier"), 1);

    client.send(InEvent::Carrier { up: false });
    collect_until(&mut effects, |e| link_connected(e, false)).await;
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        link_connected(e, true)
    })
    .await;
    assert_eq!(handle.connect_call_count("mock://carrier"), 1);

    client.send(InEvent::Carrier { up: true });
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.connect_call_count("mock://carrier"), 2);
}

/// A connector-scoped DPMS event reaches only the TV mapped to exactly
/// that connector: TVs mapped elsewhere skip it, and so do unmapped
/// TVs (only global events — no connector — reach those).
#[tokio::test]
async fn connector_scoping_targets_the_mapped_tv() {
    let (mut effects, client, _handle) = spawn_reactor();
    let tv_a = Uuid::new_v4();
    let tv_b = Uuid::new_v4();
    let tv_c = Uuid::new_v4();
    client.command(Command::AddTv {
        record: Box::new(tv_record(tv_a, "A", mock_config("mock://scope-a"))),
        reply: None,
    });
    client.command(Command::AddTv {
        record: Box::new(tv_record(tv_b, "B", mock_config("mock://scope-b"))),
        reply: None,
    });
    // C never gets a connector match — scoped events must pass it by.
    client.command(Command::AddTv {
        record: Box::new(tv_record(tv_c, "C", mock_config("mock://scope-c"))),
        reply: None,
    });
    // All connected.
    let mut connected = 0;
    collect_until(&mut effects, |e| {
        if link_connected(e, true) {
            connected += 1;
        }
        connected >= 3
    })
    .await;

    client.send(InEvent::ConnectorMatched {
        tv: tv_a,
        connector: "card1-HDMI-A-1".into(),
    });
    client.send(InEvent::ConnectorMatched {
        tv: tv_b,
        connector: "card1-HDMI-A-2".into(),
    });
    // Identity publishes carry the connectors now.
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/meta/identity").is_some_and(|p| p["connector"] == "card1-HDMI-A-2")
    })
    .await;

    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: Some("card1-HDMI-A-1".into()),
    });
    // Only TV A blanks — B is mapped elsewhere, C is unmapped.
    let a_prefix = format!("/tv/{tv_a}/state/power");
    let b_prefix = format!("/tv/{tv_b}/state/power");
    let c_prefix = format!("/tv/{tv_c}/state/power");
    collect_until(&mut effects, |e| match e {
        tisvlay_world::WireEffect::Publish { key, payload }
            if key.contains(&a_prefix.trim_start_matches('/').to_string()) =>
        {
            payload["state"] == "screenoff"
        }
        _ => false,
    })
    .await;
    assert_quiet(&mut effects, Duration::from_millis(300), |e| match e {
        tisvlay_world::WireEffect::Publish { key, .. } => {
            key.contains(b_prefix.trim_start_matches('/'))
                || key.contains(c_prefix.trim_start_matches('/'))
        }
        _ => false,
    })
    .await;

    // The global path still reaches the unmapped TV.
    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
        connector: None,
    });
    collect_until(&mut effects, |e| match e {
        tisvlay_world::WireEffect::Publish { key, payload }
            if key.contains(&c_prefix.trim_start_matches('/').to_string()) =>
        {
            payload["state"] == "screenoff"
        }
        _ => false,
    })
    .await;
}
