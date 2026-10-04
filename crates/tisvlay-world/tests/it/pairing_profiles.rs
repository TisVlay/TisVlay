//! Slice B world tests: pairing lifecycle, the wake path, and system-event
//! profile dispatch — all asserted on the wire-effects tap.

use crate::support;

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
    let attempt = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");

    client.command(Command::Pair {
        id,
        attempt,
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
    assert_eq!(
        complete["attempt"],
        attempt.to_string(),
        "terminal carries the attempt"
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
        attempt: Uuid::new_v4(),
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
    let mut record = tv_record(id, "Idle", config);
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
        attempt: Uuid::new_v4(),
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
        attempt: Uuid::new_v4(),
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
        record: Box::new(tv_record(id, "Wakeless", config)),
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

/// A re-pair supersedes a dial already in flight on the old credentials:
/// the stale dial's outcome is dropped, the verification dial spawns, and
/// only its connect settles the pairing. Without the supersede the stale
/// dial would publish `complete` — having verified the old credentials —
/// and the verification dial would never run.
#[tokio::test]
async fn re_pair_supersedes_an_in_flight_dial() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");

    // Enrollment's dial stalls in connect: it's in flight when the
    // re-pair lands.
    handle.set_connect_delay(Duration::from_millis(600));
    client.command(Command::AddTv {
        record: Box::new(tv_record(
            id,
            "Den",
            pairing_config("mock://superseded", "accept"),
        )),
        reply: Some(1),
    });
    collect_until(&mut effects, |e| reply_to(e, 1)).await;
    client.command(Command::RePair {
        id,
        attempt,
        reply: Some(2),
    });

    let seen = collect_until(&mut effects, |e| {
        progress_on(e, &progress_key, "complete").is_some()
    })
    .await;
    assert_eq!(
        seen.iter()
            .find_map(|e| progress_on(e, &progress_key, "complete"))
            .map(|p| p["attempt"].clone()),
        Some(serde_json::json!(attempt.to_string())),
    );
    assert_eq!(
        handle.connect_call_count("mock://superseded"),
        2,
        "the stale dial and the verification dial both connected"
    );
    // The stale dial's late outcome, if still in flight, settles nothing.
    assert_quiet(&mut effects, Duration::from_millis(800), |e| {
        progress_on(e, &progress_key, "complete").is_some()
    })
    .await;
}

fn with(mut config: toml::Value, key: &str, value: &str) -> toml::Value {
    config
        .as_table_mut()
        .expect("config is a table")
        .insert(key.into(), toml::Value::String(value.into()));
    config
}

/// The published display row for `label`, if this effect is a vendor-rows
/// publish carrying one.
fn vendor_row(effect: &WireEffect, label: &str) -> Option<String> {
    is_publish_on(effect, "/meta/vendor")?["config"]
        .as_array()?
        .iter()
        .find(|row| row[0] == label)
        .and_then(|row| row[1].as_str().map(str::to_string))
}

/// A pairing dial reads the device's link MAC and fills an unset `mac`,
/// canonicalized — and the config-derived facts refresh at once (the
/// display rows republish), not at the next dial.
#[tokio::test]
async fn pair_fills_an_unset_mac_from_the_device() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");
    client.command(Command::Pair {
        id,
        attempt: Uuid::new_v4(),
        name: "Office".into(),
        vendor: "mock".into(),
        config: with(
            pairing_config("mock://routed", "accept"),
            "link_mac",
            "84-90-0a-63-a9-94",
        ),
        managed: true,
        reply: Some(1),
    });
    let mut report = None;
    let mut filled = false;
    collect_until(&mut effects, |e| {
        if let Some(complete) = progress_on(e, &progress_key, "complete") {
            report = Some(complete["link_mac"].clone());
        }
        filled |= vendor_row(e, "MAC").as_deref() == Some("84:90:0A:63:A9:94");
        report.is_some() && filled
    })
    .await;
    assert_eq!(
        report,
        Some(serde_json::json!({
            "reported": "84:90:0A:63:A9:94",
            "outcome": { "kind": "filled" },
        }))
    );
    assert_eq!(handle.link_mac_call_count("mock://routed"), 1);
}

/// A configured MAC — typed, or auto-filled by ARP — is never overwritten
/// by the device's report.
#[tokio::test]
async fn pair_keeps_a_configured_mac() {
    let (mut effects, client, _handle) = spawn_reactor();
    let id = Uuid::new_v4();
    let progress_key = format!("tisvlay/test/pairing/{id}/progress");
    let config = with(
        with(
            pairing_config("mock://typed", "accept"),
            "mac",
            "64:E4:A5:6C:AB:B4",
        ),
        "link_mac",
        "84:90:0A:63:A9:94",
    );
    client.command(Command::Pair {
        id,
        attempt: Uuid::new_v4(),
        name: "Office".into(),
        vendor: "mock".into(),
        config,
        managed: true,
        reply: Some(1),
    });
    let seen = collect_until(&mut effects, |e| {
        progress_on(e, &progress_key, "complete").is_some()
    })
    .await;
    assert_eq!(
        seen.iter()
            .find_map(|e| progress_on(e, &progress_key, "complete"))
            .map(|p| p["link_mac"].clone()),
        Some(serde_json::json!({
            "reported": "84:90:0A:63:A9:94",
            "outcome": { "kind": "differs", "configured": "64:E4:A5:6C:AB:B4" },
        }))
    );
    assert!(
        seen.iter()
            .all(|e| vendor_row(e, "MAC").as_deref() != Some("84:90:0A:63:A9:94")),
        "the device's MAC must not replace the configured one"
    );
    assert_quiet(&mut effects, Duration::from_millis(300), |e| {
        vendor_row(e, "MAC").as_deref() == Some("84:90:0A:63:A9:94")
    })
    .await;
}

/// Only pairing dials read the link MAC: an ordinary enrollment dial
/// never asks.
#[tokio::test]
async fn plain_dial_never_reads_the_link_mac() {
    let (mut effects, client, handle) = spawn_reactor();
    let id = Uuid::new_v4();
    client.command(Command::AddTv {
        record: Box::new(tv_record(
            id,
            "Den",
            with(mock_config("mock://plain"), "link_mac", "84:90:0A:63:A9:94"),
        )),
        reply: Some(1),
    });
    collect_until(&mut effects, |e| link_connected(e, true)).await;
    assert_eq!(handle.link_mac_call_count("mock://plain"), 0);
}
