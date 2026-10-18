//! Whole-fleet views: `list` and `watch`.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{tv, vendors};
use tisvlay_protocol::keys;
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

use crate::call;

/// Hydrate from the publisher caches (AdvancedSubscriber history — the
/// cache's queryable lives under the verbatim `@adv` suffix, so plain
/// wildcard gets can't reach it) and render one line per TV.
pub(crate) async fn list(
    session: &Session,
    caller: &Caller,
    site: &str,
    json_out: bool,
) -> Result<()> {
    let subscriber = session
        .declare_subscriber(keys::tv_all(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .map_err(|e| anyhow!("subscribe: {e}"))?;
    let mut tvs: BTreeMap<Uuid, BTreeMap<String, Value>> = BTreeMap::new();
    // History replays immediately; stop after a quiet window.
    while let Ok(Ok(sample)) =
        tokio::time::timeout(Duration::from_millis(400), subscriber.recv_async()).await
    {
        if sample.kind() != SampleKind::Put {
            continue;
        }
        if let Some((id, path)) = keys::parse_tv_path(site, sample.key_expr().as_str()) {
            let value = serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
            tvs.entry(id).or_default().insert(path.to_string(), value);
        }
    }
    if json_out {
        let out: serde_json::Map<String, Value> = tvs
            .into_iter()
            .map(|(id, fields)| (id.to_string(), Value::Object(fields.into_iter().collect())))
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Object(out))?);
        return Ok(());
    }
    if tvs.is_empty() {
        println!("No TVs configured. Use 'tisvlay pair' to add one.");
        return Ok(());
    }
    println!("Configured TVs:");
    let mut descriptor_fields: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (id, fields) in &tvs {
        let identity = fields.get("meta/identity").cloned().unwrap_or(Value::Null);
        let name = identity["name"].as_str().unwrap_or("?");
        let vendor = identity["vendor"].as_str().unwrap_or("?");
        let status = if fields
            .get("state/link")
            .is_some_and(|v| v["connected"] == true)
        {
            "connected"
        } else {
            "disconnected"
        };
        let managed = if fields
            .get("meta/config")
            .is_none_or(|v| v["managed"] != false)
        {
            ""
        } else {
            " [unmanaged]"
        };
        // Backend's primary descriptor (LG: IP): the vendor schema's
        // first field names it, the redacted config carries the value.
        let descriptor = match descriptor_field(caller, vendor, &mut descriptor_fields).await {
            Some(field) => call::<tv::GetConfig>(caller, id, &())
                .await
                .ok()
                .and_then(|view| view.config.get(&field)?.as_str().map(str::to_string)),
            None => None,
        }
        .unwrap_or_else(|| "?".into());
        println!("  {id} - {name} ({descriptor}) [{status}] <{vendor}>{managed}");
    }
    Ok(())
}

/// The vendor schema's first field name — the backend's primary
/// descriptor, per the schema-ordering convention. Cached per vendor.
async fn descriptor_field(
    caller: &Caller,
    vendor: &str,
    cache: &mut BTreeMap<String, Option<String>>,
) -> Option<String> {
    if let Some(field) = cache.get(vendor) {
        return field.clone();
    }
    let field = call::<vendors::Schema>(caller, vendor, &())
        .await
        .ok()
        .and_then(|schema| schema.into_iter().next().map(|field| field.name));
    cache.insert(vendor.to_string(), field.clone());
    field
}

pub(crate) async fn watch(session: &Session, site: &str) -> Result<()> {
    let subscriber = session
        .declare_subscriber(keys::all(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .recovery(RecoveryConfig::default().heartbeat())
        .await
        .map_err(|e| anyhow!("subscribe: {e}"))?;
    println!("watching {} (ctrl-c to stop)", keys::all(site));
    loop {
        tokio::select! {
            sample = subscriber.recv_async() => match sample {
                Ok(sample) => {
                    let payload = sample
                        .payload()
                        .try_to_string()
                        .unwrap_or_else(|e| e.to_string().into());
                    println!("{:6} {} {}", sample.kind().to_string(), sample.key_expr(), payload);
                }
                Err(_) => return Ok(()),
            },
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
