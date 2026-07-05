//! Slice B world tests: pairing lifecycle, the wake path, and system-event
//! profile dispatch — all asserted on the wire-effects tap.

mod support;

use std::time::Duration;

use support::*;
use tisvlay_world::{Command, InEvent, SystemEvent, WireEffect};
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

/// The pairing terminal is gated on the first dial: a handshake that
/// succeeds against a TV the daemon can't actually connect to must
/// fail the pairing with the dial's reason and roll the TV back —
/// nothing left on the wire, nothing in the catalog.
#[tokio::test]
async fn pair_fails_and_rolls_back_when_first_dial_fails() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");

    let mut config = pairing_config("mock://pair-dead", "accept");
    config
        .as_table_mut()
        .unwrap()
        .insert("refuse_connect".into(), toml::Value::Boolean(true));
    client.command(Command::Pair {
        id,
        name: "Dead".into(),
        vendor: "mock".into(),
        config,
        managed: true,
        reply: Some(20),
    });

    let seen = collect_until(&mut effects, |e| {
        progress_on(e, &progress_key, "failed").is_some()
    })
    .await;
    let failed = seen
        .iter()
        .find_map(|e| progress_on(e, &progress_key, "failed"))
        .unwrap();
    assert_eq!(failed["code"], "pairing.connection-failed");
    assert!(
        !seen
            .iter()
            .any(|e| progress_on(e, &progress_key, "complete").is_some()),
        "no complete may precede the dial outcome"
    );

    // Rollback: the TV's published keys are swept…
    let link_key = format!("tisvlay/test/tv/{id}/state/link");
    collect_until(
        &mut effects,
        |e| matches!(e, WireEffect::Delete { key } if *key == link_key),
    )
    .await;
    // …and the entity is gone from the catalog.
    client.command(Command::SetVolume {
        id,
        level: 1,
        reply: Some(21),
    });
    let seen = collect_until(&mut effects, |e| reply_to(e, 21)).await;
    assert_eq!(
        seen.iter().find_map(|e| reply_err_code(e, 21)),
        Some("tv-not-found")
    );
}

/// Config-derived wake support reaches the wire at enrollment — no
/// successful dial required. This TV's dial always fails, yet its
/// link must still say it can be woken (the mock always can), so
/// clients don't misreport "no wake path" for TVs that never
/// connected.
#[tokio::test]
async fn transport_facts_reach_the_link_without_a_dial() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let mut config = pairing_config("mock://idle", "manual");
    config
        .as_table_mut()
        .unwrap()
        .insert("refuse_connect".into(), toml::Value::Boolean(true));
    let mut record = support::tv_record(id, "Idle", config);
    record.managed = false;

    client.command(Command::AddTv {
        record: Box::new(record),
        reply: Some(30),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/link")
            .is_some_and(|p| p["supports_wake"] == true && p["connected"] == false)
    })
    .await;
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
        restore_input: false,
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
        connector: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/power").is_some_and(|p| p["state"] == "screenoff")
    })
    .await;

    client.send(InEvent::System {
        event: SystemEvent::DpmsOn,
        connector: None,
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

/// A vendor-config patch refreshes transport facts even when no dial
/// follows: adding wake capability to a TV whose retry budget is spent
/// (off and unreachable — exactly the state you add a MAC in) must
/// light `supports_wake` up immediately, not at the next successful
/// dial.
#[tokio::test]
async fn config_patch_refreshes_wake_support_without_a_dial() {
    let (mut effects, client, handle) = spawn_reactor();
    let base = std::time::Instant::now();
    let id = Uuid::new_v4();
    let mut config = pairing_config("mock://wakeless", "manual");
    let table = config.as_table_mut().unwrap();
    table.insert("refuse_connect".into(), toml::Value::Boolean(true));
    table.insert("wake".into(), toml::Value::Boolean(false));
    client.command(Command::AddTv {
        record: Box::new(support::tv_record(id, "Wakeless", config)),
        reply: None,
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/link")
            .is_some_and(|p| p["supports_wake"] == false && p["connected"] == false)
    })
    .await;

    // Exhaust the dial budget so the TV sits disconnected with no
    // pending dial — the state where the old facts path never fired.
    for attempt in 1..=u64::from(tisvlay_world::CONNECT_MAX_ATTEMPTS) {
        client.tick(base + tisvlay_world::CONNECT_BACKOFF * u32::try_from(attempt).unwrap() * 2);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let dials_before = handle.connect_call_count("mock://wakeless");

    client.command(Command::UpdateConfig {
        id,
        patch: serde_json::json!({ "wake": true }),
        reply: Some(31),
    });
    collect_until(&mut effects, |e| {
        is_publish_on(e, "/state/link").is_some_and(|p| p["supports_wake"] == true)
    })
    .await;
    // The facts came from the patch, not from a fresh dial.
    assert_eq!(handle.connect_call_count("mock://wakeless"), dials_before);
}
