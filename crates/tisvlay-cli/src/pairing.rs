//! Discovery, pairing and re-pairing: the verbs that find TVs and enroll them.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{manager, vendors};
use tisvlay_protocol::{keys, payload, rpc};
use tokio::signal::unix::{Signal, SignalKind, signal};
use uuid::Uuid;
use zenoh::Session;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use crate::call;
use crate::render::print_reply;

/// Listen for ctrl-c (SIGINT) from here on. Creating the stream installs
/// the handler at once, where `tokio::signal::ctrl_c()` installs it only
/// when first polled: a ctrl-c between a printed "ctrl-c stops" and that
/// first poll would kill the process with SIGINT's default action.
fn interrupt() -> Result<Signal> {
    signal(SignalKind::interrupt()).context("listen for ctrl-c")
}

/// Start an open-ended discovery session, stream findings, stop on
/// ctrl-c. `--json` emits one JSON object per finding (JSONL); the
/// session's start and stop notes go to stderr.
pub(crate) async fn discover_session(
    session: &Session,
    caller: &Caller,
    site: &str,
    vendor: Option<String>,
    json_out: bool,
) -> Result<()> {
    let mut interrupt = interrupt()?;
    let id = call::<manager::DiscoverStart>(caller, &(), &rpc::DiscoverStart { vendor })
        .await?
        .session;
    eprintln!("discovery session {id} — ctrl-c stops");
    let sub = session
        .declare_subscriber(format!("tisvlay/{site}/discovery/{id}/found/*"))
        .await
        .map_err(|e| anyhow!("subscribe findings: {e}"))?;
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    loop {
        tokio::select! {
            sample = sub.recv_async() => {
                let Ok(sample) = sample else { break };
                let tv: Value = serde_json::from_slice(&sample.payload().to_bytes())
                    .unwrap_or(Value::Null);
                let finding = tv["id"].as_str().unwrap_or_default().to_string();
                if seen.insert(finding, ()).is_none() {
                    if json_out {
                        println!("{tv}");
                        continue;
                    }
                    let mac = tv["vendor_data"]["mac"]
                        .as_str()
                        .map(|mac| format!("  mac={mac}"))
                        .unwrap_or_default();
                    println!(
                        "{}  {}  {}  id={}{}",
                        tv["vendor"].as_str().unwrap_or("?"),
                        tv["name"].as_str().unwrap_or("?"),
                        tv["address"].as_str().unwrap_or("?"),
                        tv["id"].as_str().unwrap_or("?"),
                        mac,
                    );
                }
            }
            _ = interrupt.recv() => break,
        }
    }
    call::<manager::DiscoverStop>(caller, &(), &rpc::DiscoverStop { session: id }).await?;
    eprintln!("stopped");
    Ok(())
}

/// A bounded scan. With no `timeout`, the daemon applies its own window.
pub(crate) async fn discover(
    caller: &Caller,
    vendor: Option<String>,
    timeout: Option<Duration>,
    json_out: bool,
) -> Result<()> {
    let timeout_ms = timeout.map(|window| {
        u64::try_from(window.as_millis())
            .expect("the scan window parser caps it at seconds, far under u64::MAX ms")
    });
    let request = rpc::Discover { vendor, timeout_ms };
    let found = caller.call_many::<manager::Discover>(&(), &request).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::to_value(&found)?)?
        );
        return Ok(());
    }
    if found.is_empty() {
        println!("nothing found");
        return Ok(());
    }
    for tv in found {
        let mac = tv
            .vendor_data
            .get("mac")
            .map(|mac| format!("  mac={mac}"))
            .unwrap_or_default();
        println!(
            "{}  {}  {}  id={}{}",
            tv.vendor, tv.name, tv.address, tv.id, mac,
        );
    }
    Ok(())
}

/// `vendor`: the backend catalog, or one backend's catalog row and its
/// pairing fields (`--json`: the row with a `schema` array).
pub(crate) async fn vendor(caller: &Caller, id: Option<String>, json_out: bool) -> Result<()> {
    let catalog = call::<vendors::Catalog>(caller, &(), &()).await?;
    let Some(id) = id else {
        if json_out {
            return print_reply(&catalog, true);
        }
        for vendor in catalog {
            println!(
                "{}  {}  [{}]",
                vendor.id,
                vendor.name,
                vendor.capabilities.join(", "),
            );
        }
        return Ok(());
    };
    // The schema rpc refuses an unknown vendor with a typed error.
    let schema = call::<vendors::Schema>(caller, id.as_str(), &()).await?;
    let Some(vendor) = catalog.into_iter().find(|vendor| vendor.id == id) else {
        bail!("vendor '{id}' has a schema but no catalog entry");
    };
    if json_out {
        let mut row = serde_json::to_value(&vendor)?;
        row["schema"] = serde_json::to_value(&schema)?;
        return print_reply(&row, true);
    }
    println!(
        "{}  {}  [{}]",
        vendor.id,
        vendor.name,
        vendor.capabilities.join(", "),
    );
    for field in schema {
        let required = if field.required { " (required)" } else { "" };
        println!("  {}{}  {}", field.name, required, field.helper);
    }
    Ok(())
}

fn pairing_config(params: &str) -> Result<serde_json::Map<String, Value>> {
    serde_json::from_str(params).context("--params must be a JSON object")
}

/// Pair a new TV: progress on stderr, its UUID on stdout once paired.
pub(crate) async fn pair(
    session: &Session,
    caller: &Caller,
    site: &str,
    name: String,
    vendor: String,
    params: &str,
    managed: bool,
) -> Result<()> {
    let request = rpc::AddTv {
        id: None,
        name,
        vendor,
        managed,
        config: pairing_config(params)?,
    };
    let started = call::<manager::Pair>(caller, &(), &request).await?;
    eprintln!("pairing session {} started", started.id);
    watch_pairing(session, caller, site, started.id, started.attempt).await
}

/// One line on what the pairing did with the device-reported link MAC.
fn link_mac_line(id: Uuid, report: &payload::LinkMacReport) -> String {
    use tisvlay_protocol::payload::LinkMacOutcome;
    let reported = &report.reported;
    match &report.outcome {
        LinkMacOutcome::Filled => format!("mac: detected {reported} (saved)"),
        LinkMacOutcome::Matches => format!("mac: {reported} (matches the TV)"),
        LinkMacOutcome::Differs { configured } => format!(
            "mac: TV reports {reported}, configured {configured}; to use it: \
             tisvlay config {id} '{{\"mac\":\"{reported}\"}}'"
        ),
    }
}

fn is_current_pairing_attempt(progress: &payload::PairingProgress, attempt: Uuid) -> bool {
    progress.attempt == attempt
}

/// Stream a pairing attempt's progress phases to stderr until a terminal
/// one, then print the TV's UUID to stdout; ctrl-c sends cancel-pair.
/// Samples from other attempts (earlier sessions replayed from the cache)
/// are skipped.
pub(crate) async fn watch_pairing(
    session: &Session,
    caller: &Caller,
    site: &str,
    id: Uuid,
    attempt: Uuid,
) -> Result<()> {
    let subscriber = session
        .declare_subscriber(keys::pairing_progress(site, &id))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe progress: {e}"))?;
    let mut interrupt = interrupt()?;
    loop {
        tokio::select! {
            sample = subscriber.recv_async() => {
                let Ok(sample) = sample else { bail!("progress stream closed") };
                let Ok(progress) =
                    serde_json::from_slice::<payload::PairingProgress>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                if !is_current_pairing_attempt(&progress, attempt) {
                    continue;
                }
                eprintln!("[{}] {}", progress.phase, progress.message);
                match progress.phase {
                    payload::PairingPhase::Complete => {
                        if let Some(report) = &progress.link_mac {
                            eprintln!("{}", link_mac_line(id, report));
                        }
                        println!("{id}");
                        return Ok(());
                    }
                    payload::PairingPhase::Failed => bail!(
                        "pairing failed: {}",
                        progress.code.map_or("unknown", payload::FailureCode::as_str)
                    ),
                    payload::PairingPhase::AwaitingTv | payload::PairingPhase::AwaitingInput => {}
                }
            }
            _ = interrupt.recv() => {
                eprintln!("cancelling…");
                let _ = call::<manager::CancelPair>(caller, &(), &rpc::TvRef { id }).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn link_mac_line_names_each_outcome() {
        use tisvlay_protocol::payload::{LinkMacOutcome, LinkMacReport};
        let id = uuid::Uuid::nil();
        let report = |outcome| LinkMacReport {
            reported: "84:90:0A:63:A9:94".into(),
            outcome,
        };
        assert_eq!(
            super::link_mac_line(id, &report(LinkMacOutcome::Filled)),
            "mac: detected 84:90:0A:63:A9:94 (saved)"
        );
        assert_eq!(
            super::link_mac_line(id, &report(LinkMacOutcome::Matches)),
            "mac: 84:90:0A:63:A9:94 (matches the TV)"
        );
        assert_eq!(
            super::link_mac_line(
                id,
                &report(LinkMacOutcome::Differs {
                    configured: "64:E4:A5:6C:AB:B4".into()
                })
            ),
            format!(
                "mac: TV reports 84:90:0A:63:A9:94, configured 64:E4:A5:6C:AB:B4; \
                 to use it: tisvlay config {id} '{{\"mac\":\"84:90:0A:63:A9:94\"}}'"
            )
        );
    }

    #[test]
    fn stale_pairing_progress_is_rejected_by_attempt_id() {
        use tisvlay_protocol::payload::{PairingPhase, PairingProgress};

        let current = uuid::Uuid::new_v4();
        let stale = PairingProgress {
            attempt: uuid::Uuid::new_v4(),
            phase: PairingPhase::Complete,
            message: "paired".into(),
            code: None,
            path: None,
            link_mac: None,
        };
        let live = PairingProgress {
            attempt: current,
            ..stale.clone()
        };

        assert!(!super::is_current_pairing_attempt(&stale, current));
        assert!(super::is_current_pairing_attempt(&live, current));
    }

    #[test]
    fn pairing_config_takes_a_json_object_only() {
        let params =
            super::pairing_config(r#"{"ip":"192.0.2.4"}"#).expect("JSON params are accepted");
        assert_eq!(
            params.get("ip").and_then(serde_json::Value::as_str),
            Some("192.0.2.4")
        );
        assert!(
            format!(
                "{:#}",
                super::pairing_config("[]").expect_err("params must decode as an object")
            )
            .contains("--params must be a JSON object")
        );
    }
}
