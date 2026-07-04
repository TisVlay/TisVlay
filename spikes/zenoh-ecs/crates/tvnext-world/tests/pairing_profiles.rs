//! Slice B world tests: pairing lifecycle, the wake path, and system-event
//! profile dispatch — all asserted on the wire-effects tap.

mod support;

use std::time::Duration;

use support::*;
use tvnext_world::{Command, InEvent, SystemEvent, WireEffect};
use uuid::Uuid;

fn pairing_config(address: &str, mode: &str) -> toml::Value {
    let mut table = toml::value::Table::new();
    table.insert("address".into(), toml::Value::String(address.into()));
    table.insert("pairing".into(), toml::Value::String(mode.into()));
    toml::Value::Table(table)
}

fn progress_on<'a>(
    effect: &'a WireEffect,
    key: &str,
    phase: &str,
) -> Option<&'a serde_json::Value> {
    match effect {
        WireEffect::Publish { key: k, payload } if k == key && payload["phase"] == phase => {
            Some(payload)
        }
        _ => None,
    }
}

#[tokio::test]
async fn pair_accept_creates_tv_with_redacted_progress() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");

    client.command(Command::Pair {
        id,
        name: "Paired".into(),
        vendor: "mock".into(),
        config: pairing_config("mock://pair1", "accept"),
        managed: true,
        reply: Some(10),
    });

    // The paired TV comes up managed: pairing completes, the TV entity
    // spawns, dials, and connects.
    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;

    assert!(
        seen.iter().any(|e| matches!(
            e,
            WireEffect::Reply {
                to: 10,
                result: Ok(_)
            }
        )),
        "Pair must reply accepted"
    );
    let complete = seen
        .iter()
        .find_map(|e| progress_on(e, &progress_key, "complete"))
        .expect("terminal complete progress");
    assert!(
        complete.get("credentials").is_none() && !complete.to_string().contains("mock-key"),
        "credentials must never reach the wire: {complete}"
    );
    assert!(
        seen.iter()
            .any(|e| is_publish_on(e, "/meta/identity").is_some_and(|p| p["name"] == "Paired")),
        "paired TV's identity published"
    );
}

#[tokio::test]
async fn pair_reject_publishes_typed_failure_and_no_tv() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");

    client.command(Command::Pair {
        id,
        name: "Rejected".into(),
        vendor: "mock".into(),
        config: pairing_config("mock://pair2", "reject"),
        managed: true,
        reply: Some(11),
    });

    let seen = collect_until(&mut effects, |e| {
        progress_on(e, &progress_key, "failed").is_some()
    })
    .await;
    let failed = seen
        .iter()
        .find_map(|e| progress_on(e, &progress_key, "failed"))
        .unwrap();
    assert_eq!(failed["code"], "pairing.rejected");

    // No TV entity was created.
    client.command(Command::SetVolume {
        id,
        level: 1,
        reply: Some(12),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 12)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 12)),
        Some("tv-not-found")
    );
}

#[tokio::test]
async fn cancel_pair_terminates_manual_session() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");

    // "manual" mode waits indefinitely for an outcome — cancel is the only
    // way out.
    client.command(Command::Pair {
        id,
        name: "Cancelled".into(),
        vendor: "mock".into(),
        config: pairing_config("mock://pair3", "manual"),
        managed: true,
        reply: Some(13),
    });
    collect_until(&mut effects, |e| {
        progress_on(e, &progress_key, "awaiting_tv").is_some()
    })
    .await;

    client.command(Command::CancelPair {
        id,
        reply: Some(14),
    });
    let seen = collect_until(&mut effects, |e| {
        progress_on(e, &progress_key, "failed").is_some()
    })
    .await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            WireEffect::Reply {
                to: 14,
                result: Ok(_)
            }
        )),
        "CancelPair replies ok"
    );
    let failed = seen
        .iter()
        .find_map(|e| progress_on(e, &progress_key, "failed"))
        .unwrap();
    assert_eq!(failed["code"], "pairing.cancelled");
}

/// PowerOn against a disconnected, wake-capable TV takes the wake path
/// (`wake_and_connect`) instead of erroring — the WoL flow's shape.
#[tokio::test]
async fn power_on_wakes_disconnected_tv() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = add_connected_tv(&mut effects, &client, "mock://wake").await;
    assert_eq!(handle.connect_call_count("mock://wake"), 1);

    client.command(Command::PowerOff { id, reply: None });
    collect_until(&mut effects, |e| link_connected(e, false)).await;

    client.command(Command::PowerOn {
        id,
        reply: Some(20),
    });
    let seen = collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            WireEffect::Reply {
                to: 20,
                result: Ok(_)
            }
        )),
        "wake-path PowerOn replies accepted"
    );
    assert_eq!(handle.connect_call_count("mock://wake"), 2);
    assert!(
        seen.iter()
            .any(|e| is_publish_on(e, "/state/power").is_some_and(|p| p["is_on"] == true)),
        "TV reports on after wake"
    );
}

/// System events flow through the profile to every managed, connected TV:
/// DPMS off blanks the panel, DPMS on restores it.
#[tokio::test]
async fn system_events_dispatch_profile_actions() {
    let (mut effects, client, _handle) = spawn_reactor();
    add_connected_tv(&mut effects, &client, "mock://prof").await;

    client.send(InEvent::System {
        event: SystemEvent::DpmsOff,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;

    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power")
            .is_some_and(|p| p["state"] == "active" && p["is_screen_on"] == true)
    })
    .await;

    // Sanity: quiet afterwards — no action loops.
    assert_quiet(&mut effects, Duration::from_millis(200), |e| {
        is_publish_on(e, "/state/power").is_some()
    })
    .await;
}
