//! Discovery, pairing and re-pairing: the verbs that find TVs and enroll them.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::manager;
use tisvlay_protocol::{keys, payload, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use crate::call;

/// Start an open-ended discovery session, stream findings, stop on
/// ctrl-c. `--json` emits one JSON object per finding (JSONL).
pub(crate) async fn discover_session(
    session: &Session,
    caller: &Caller,
    site: &str,
    vendor: Option<String>,
    json_out: bool,
) -> Result<()> {
    let id = call::<manager::DiscoverStart>(caller, &(), &rpc::DiscoverStart { vendor })
        .await?
        .session;
    if !json_out {
        println!("discovery session {id} — ctrl-c stops");
    }
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
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    call::<manager::DiscoverStop>(caller, &(), &rpc::DiscoverStop { session: id }).await?;
    if !json_out {
        println!("stopped");
    }
    Ok(())
}

pub(crate) async fn discover(
    caller: &Caller,
    vendor: Option<String>,
    timeout_ms: u64,
    json_out: bool,
) -> Result<()> {
    let request = rpc::Discover {
        vendor,
        timeout_ms: Some(timeout_ms),
    };
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

/// A vendor config naming only an address (the mock shorthand).
pub(crate) fn address_config(address: String) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([("address".to_string(), Value::String(address))])
}

pub(crate) fn pairing_config(
    params: Option<String>,
    address: Option<String>,
) -> Result<serde_json::Map<String, Value>> {
    match (params, address) {
        (Some(params), _) => {
            serde_json::from_str(&params).context("--params must be a JSON object")
        }
        (None, Some(address)) => Ok(address_config(address)),
        (None, None) => bail!("provide --params or --address"),
    }
}

pub(crate) async fn pair(
    session: &Session,
    caller: &Caller,
    site: &str,
    name: String,
    vendor: String,
    config: serde_json::Map<String, Value>,
) -> Result<()> {
    let request = rpc::AddTv {
        id: None,
        name,
        vendor,
        managed: true,
        config,
    };
    let started = call::<manager::Pair>(caller, &(), &request).await?;
    println!("pairing session {} started", started.id);
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
             tisvlay config-set {id} '{{\"mac\":\"{reported}\"}}'"
        ),
    }
}

fn is_current_pairing_attempt(progress: &payload::PairingProgress, attempt: Uuid) -> bool {
    progress.attempt == attempt
}

/// Stream a pairing attempt's progress phases to stdout until a terminal
/// one; ctrl-c sends cancel-pair. Samples from other attempts (earlier
/// sessions replayed from the cache) are skipped.
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
                println!("[{}] {}", progress.phase, progress.message);
                match progress.phase {
                    payload::PairingPhase::Complete => {
                        println!("paired: {id}");
                        if let Some(report) = &progress.link_mac {
                            println!("{}", link_mac_line(id, report));
                        }
                        return Ok(());
                    }
                    payload::PairingPhase::Failed => bail!(
                        "pairing failed: {}",
                        progress.code.map_or("unknown", payload::FailureCode::as_str)
                    ),
                    payload::PairingPhase::AwaitingTv | payload::PairingPhase::AwaitingInput => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("cancelling…");
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
                 to use it: tisvlay config-set {id} '{{\"mac\":\"84:90:0A:63:A9:94\"}}'"
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
    fn pairing_config_prefers_json_params_and_supports_address_shortcut() {
        let params = super::pairing_config(
            Some(r#"{"ip":"192.0.2.4"}"#.into()),
            Some("mock://ignored".into()),
        )
        .expect("JSON params are accepted");
        assert_eq!(
            params.get("ip").and_then(serde_json::Value::as_str),
            Some("192.0.2.4")
        );
        assert!(!params.contains_key("address"));

        let address = super::pairing_config(None, Some("mock://den".into()))
            .expect("address shortcut is accepted");
        assert_eq!(
            address.get("address").and_then(serde_json::Value::as_str),
            Some("mock://den")
        );
    }

    #[test]
    fn pairing_config_rejects_missing_or_non_object_params() {
        assert_eq!(
            super::pairing_config(None, None)
                .expect_err("one pairing config source is required")
                .to_string(),
            "provide --params or --address"
        );
        assert!(
            format!(
                "{:#}",
                super::pairing_config(Some("[]".into()), None)
                    .expect_err("params must decode as an object")
            )
            .contains("--params must be a JSON object")
        );
    }
}
