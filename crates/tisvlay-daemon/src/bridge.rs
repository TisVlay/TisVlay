//! Zenoh ↔ world bridge: wire effects out (cached publishes, deletes,
//! query replies), RPC queries in (world commands with reply correlation).
//!
//! One task owns everything — the publisher map, the pending-reply map,
//! and the pairing liveliness tokens — so there are no locks here either.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tisvlay_wire::{keys, rpc};
use tisvlay_world::{Command, ReplyTo, WireEffect, WireError, WorldClient};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::discovery::DynDiscovery;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::liveliness::LivelinessToken;
use zenoh::query::{Query, Queryable};
use zenoh_ext::{AdvancedPublisher, AdvancedPublisherBuilderExt, CacheConfig, MissDetectionConfig};

/// Cache depth for pairing-progress keys — deep enough that a re-opened
/// wizard replays the whole session.
const PAIRING_HISTORY: usize = 64;

/// Heartbeat period for sample-miss recovery on cached publishers.
const MISS_HEARTBEAT: Duration = Duration::from_millis(500);

/// One vendor's catalog entry: identity, capabilities, and the connector
/// whose schema the catalog queryables serve.
pub(crate) struct VendorMeta {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub connector: Arc<dyn tisvlay_core::TvConnector>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    session: Session,
    site: String,
    mut effects: mpsc::UnboundedReceiver<WireEffect>,
    world: WorldClient,
    discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>>,
    catalog: Arc<Vec<VendorMeta>>,
    info: Value,
    ready: tokio::sync::oneshot::Sender<()>,
) -> anyhow::Result<()> {
    let (query_tx, mut queries) = mpsc::unbounded_channel::<Query>();
    let mut queryables: Vec<Queryable<()>> = Vec::new();
    for key in [
        keys::manager_rpc_wild(&site),
        keys::tv_rpc_wild(&site),
        keys::profile_rpc_wild(&site),
        keys::vendors_wild(&site),
        keys::vendors(&site),
    ] {
        let tx = query_tx.clone();
        let queryable = session
            .declare_queryable(key.as_str())
            .complete(true)
            .callback(move |query| {
                let _ = tx.send(query);
            })
            .await
            .map_err(|e| anyhow::anyhow!("declare rpc queryable {key}: {e}"))?;
        queryables.push(queryable);
    }
    // Serve-readiness gate: `serve` withholds the daemon's liveliness
    // token until this fires, so no client can observe the daemon
    // alive while the rpc surface is still being declared.
    let _ = ready.send(());

    let mut bridge = Bridge {
        session,
        site,
        world,
        discoveries,
        catalog,
        info,
        publishers: HashMap::new(),
        pending: HashMap::new(),
        next_reply: 1,
        pairing_tokens: HashMap::new(),
        discovery_sessions: HashMap::new(),
        vendors_by_tv: HashMap::new(),
    };

    loop {
        tokio::select! {
            effect = effects.recv() => match effect {
                None => return Ok(()),
                Some(effect) => bridge.handle_effect(effect).await,
            },
            query = queries.recv() => match query {
                None => return Ok(()),
                Some(query) => bridge.handle_query(query).await,
            },
        }
    }
}

struct PendingQuery {
    query: Query,
    /// Reply payload overriding the world's plain `{}` ack — carries the
    /// generated uuid for add-tv / pair.
    ok_payload: Option<Value>,
}

struct Bridge {
    session: Session,
    site: String,
    world: WorldClient,
    discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>>,
    catalog: Arc<Vec<VendorMeta>>,
    info: Value,
    publishers: HashMap<String, AdvancedPublisher<'static>>,
    pending: HashMap<ReplyTo, PendingQuery>,
    next_reply: ReplyTo,
    pairing_tokens: HashMap<String, LivelinessToken>,
    discovery_sessions: HashMap<Uuid, tokio::task::JoinHandle<()>>,
    /// TV → vendor, learned from the identity payloads this bridge
    /// publishes. Lets `update-config` validate vendor fields against
    /// the right schema without a world round-trip.
    vendors_by_tv: HashMap<Uuid, String>,
}

impl Bridge {
    async fn handle_effect(&mut self, effect: WireEffect) {
        match effect {
            WireEffect::Publish { key, payload } => {
                self.update_pairing_liveliness(&key, &payload).await;
                if let Some((tv, "meta/identity")) = keys::parse_tv_path(&self.site, &key)
                    && let Some(vendor) = payload["vendor"].as_str()
                {
                    self.vendors_by_tv.insert(tv, vendor.to_string());
                }
                self.publish(&key, &payload).await;
            }
            WireEffect::Delete { key } => {
                if let Some((tv, "meta/identity")) = keys::parse_tv_path(&self.site, &key) {
                    self.vendors_by_tv.remove(&tv);
                }
                match self.publishers.remove(&key) {
                    // Delete through the owning publisher so its cache records
                    // the tombstone, then drop it — late joiners see nothing.
                    Some(publisher) => {
                        if let Err(e) = publisher.delete().await {
                            warn!(%key, %e, "delete failed");
                        }
                    }
                    None => {
                        if let Err(e) = self.session.delete(key.as_str()).await {
                            warn!(%key, %e, "session delete failed");
                        }
                    }
                }
            }
            WireEffect::Reply { to, result } => {
                let Some(pending) = self.pending.remove(&to) else {
                    warn!(to, "reply for unknown query dropped");
                    return;
                };
                match result {
                    Ok(value) => {
                        let payload = pending.ok_payload.unwrap_or(value);
                        let bytes = serde_json::to_vec(&payload).unwrap_or_default();
                        let key = pending.query.key_expr().clone().into_owned();
                        if let Err(e) = pending
                            .query
                            .reply(key, bytes)
                            .encoding(Encoding::APPLICATION_JSON)
                            .await
                        {
                            warn!(to, %e, "reply failed");
                        }
                    }
                    Err(error) => reply_err(&pending.query, &error).await,
                }
            }
        }
    }

    async fn publish(&mut self, key: &str, payload: &Value) {
        if !self.publishers.contains_key(key) {
            let depth = if keys::parse_pairing_progress(&self.site, key).is_some() {
                PAIRING_HISTORY
            } else {
                1
            };
            let declared = self
                .session
                .declare_publisher(key.to_string())
                .encoding(Encoding::APPLICATION_JSON)
                .cache(CacheConfig::default().max_samples(depth))
                .sample_miss_detection(MissDetectionConfig::default().heartbeat(MISS_HEARTBEAT))
                .publisher_detection()
                .await;
            match declared {
                Ok(publisher) => {
                    self.publishers.insert(key.to_string(), publisher);
                }
                Err(e) => {
                    warn!(%key, %e, "declare publisher failed, sample dropped");
                    return;
                }
            }
        }
        let publisher = self.publishers.get(key).expect("just inserted");
        match serde_json::to_vec(payload) {
            Ok(bytes) => {
                if let Err(e) = publisher.put(bytes).await {
                    warn!(%key, %e, "publish failed");
                }
            }
            Err(e) => warn!(%key, %e, "payload encode failed"),
        }
    }

    /// A pairing session is alive from its first progress sample to its
    /// terminal one — surfaced as a liveliness token so clients detect
    /// daemon-side aborts (token drops on crash for free).
    async fn update_pairing_liveliness(&mut self, key: &str, payload: &Value) {
        let Some(tv) = keys::parse_pairing_progress(&self.site, key) else {
            return;
        };
        let alive_key = keys::alive_pairing(&self.site, &tv);
        let terminal = matches!(
            payload.get("phase").and_then(Value::as_str),
            Some("complete" | "failed")
        );
        if terminal {
            self.pairing_tokens.remove(&alive_key);
        } else if !self.pairing_tokens.contains_key(&alive_key) {
            match self
                .session
                .liveliness()
                .declare_token(alive_key.as_str())
                .await
            {
                Ok(token) => {
                    self.pairing_tokens.insert(alive_key, token);
                }
                Err(e) => warn!(%alive_key, %e, "pairing liveliness token failed"),
            }
        }
    }

    async fn handle_query(&mut self, query: Query) {
        let key = query.key_expr().as_str().to_string();
        let body: Value = match query.payload() {
            Some(bytes) => match serde_json::from_slice(&bytes.to_bytes()) {
                Ok(value) => value,
                Err(e) => {
                    reply_err(&query, &bad_request(format!("invalid JSON body: {e}"))).await;
                    return;
                }
            },
            None => Value::Null,
        };
        // Catalog reads, introspection, and discovery are served
        // bridge-side, never touching the world.
        if key == keys::manager_rpc(&self.site, "info") {
            let info = self.info.clone();
            reply_json(&query, &info).await;
            return;
        }
        if key == keys::manager_rpc(&self.site, "enrich") {
            self.handle_enrich(query, &body).await;
            return;
        }
        if key == keys::vendors(&self.site) {
            let list: Vec<Value> = self
                .catalog
                .iter()
                .map(|v| json!({ "id": v.id, "name": v.name, "capabilities": v.capabilities }))
                .collect();
            reply_json(&query, &Value::Array(list)).await;
            return;
        }
        if let Some(rest) = key.strip_prefix(&format!("tisvlay/{}/manager/vendors/", self.site))
            && let Some(vendor) = rest.strip_suffix("/schema")
        {
            match self.catalog.iter().find(|v| v.id == vendor) {
                Some(meta) => {
                    let schema =
                        serde_json::to_value(meta.connector.pairing_schema()).unwrap_or_default();
                    reply_json(&query, &schema).await;
                }
                None => reply_err(&query, &WireError::unknown_vendor(vendor)).await,
            }
            return;
        }
        match keys::parse_manager_rpc(&self.site, &key) {
            Some("discover") => {
                self.handle_discover(query, &body);
                return;
            }
            Some("discover-start") => {
                self.handle_discover_start(query, &body).await;
                return;
            }
            Some("discover-stop") => {
                self.handle_discover_stop(query, &body).await;
                return;
            }
            _ => {}
        }
        let reply_id = self.next_reply;
        self.next_reply += 1;
        match self.route(&key, &body, reply_id).await {
            Ok((command, ok_payload)) => {
                debug!(%key, reply_id, "rpc routed");
                self.pending
                    .insert(reply_id, PendingQuery { query, ok_payload });
                if !self.world.command(command)
                    && let Some(pending) = self.pending.remove(&reply_id)
                {
                    reply_err(
                        &pending.query,
                        &WireError {
                            code: "daemon-shutting-down".into(),
                            message: "world reactor is gone".into(),
                        },
                    )
                    .await;
                }
            }
            Err(error) => reply_err(&query, &error).await,
        }
    }

    /// Open-ended scan session: publish findings until stopped, with a
    /// liveliness token marking the session alive (dropped tokens tell
    /// wizards the daemon or session died).
    async fn handle_discover_start(&mut self, query: Query, body: &Value) {
        let request: rpc::DiscoverStart = match parse(body) {
            Ok(request) => request,
            Err(error) => {
                reply_err(&query, &error).await;
                return;
            }
        };
        if let Some(vendor) = &request.vendor
            && !self.discoveries.contains_key(vendor)
        {
            reply_err(&query, &WireError::unknown_vendor(vendor)).await;
            return;
        }
        let session_id = Uuid::new_v4();
        let discoveries = Arc::clone(&self.discoveries);
        let zenoh = self.session.clone();
        let site = self.site.clone();
        let handle = tokio::spawn(async move {
            let alive = zenoh
                .liveliness()
                .declare_token(keys::alive_discovery(&site, &session_id).as_str())
                .await;
            if let Err(e) = &alive {
                warn!(%e, "discovery session liveliness failed");
            }
            loop {
                for (vendor, discovery) in discoveries.iter() {
                    if request.vendor.as_deref().is_some_and(|v| v != vendor) {
                        continue;
                    }
                    match discovery.discover(Duration::from_secs(3)).await {
                        Ok(found) => {
                            for mut tv in found {
                                if !tv.vendor_data.contains_key("mac")
                                    && let Some(mac) = crate::arp::lookup_mac(&tv.address).await
                                {
                                    tv.vendor_data.insert("mac".into(), mac);
                                }
                                let Ok(bytes) = serde_json::to_vec(&tv) else {
                                    continue;
                                };
                                let key = keys::discovery_found(&site, &session_id, &tv.id);
                                if let Err(e) = zenoh
                                    .put(key.as_str(), bytes)
                                    .encoding(Encoding::APPLICATION_JSON)
                                    .await
                                {
                                    warn!(%e, "discovery finding publish failed");
                                }
                            }
                        }
                        Err(error) => warn!(vendor, %error, "discovery scan failed"),
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        self.discovery_sessions.insert(session_id, handle);
        reply_json(&query, &json!({ "session": session_id })).await;
    }

    async fn handle_discover_stop(&mut self, query: Query, body: &Value) {
        let request: rpc::DiscoverStop = match parse(body) {
            Ok(request) => request,
            Err(error) => {
                reply_err(&query, &error).await;
                return;
            }
        };
        match self.discovery_sessions.remove(&request.session) {
            Some(handle) => {
                // Aborting drops the task's liveliness token with it.
                handle.abort();
                reply_json(&query, &json!({})).await;
            }
            None => {
                reply_err(
                    &query,
                    &WireError {
                        code: "discovery-not-found".into(),
                        message: format!("no discovery session {}", request.session),
                    },
                )
                .await;
            }
        }
    }

    /// Manual-entry enrichment: probe the ARP cache for `ip` (after the
    /// vendor primes it) so the wizard can hint the device's MAC before
    /// pairing. Best-effort — an empty map just means no hint.
    async fn handle_enrich(&self, query: Query, body: &Value) {
        let vendor = body["vendor"].as_str().unwrap_or_default().to_string();
        let ip = body["ip"]
            .as_str()
            .or_else(|| body["address"].as_str())
            .unwrap_or_default()
            .to_string();
        let connector = self
            .catalog
            .iter()
            .find(|meta| meta.id == vendor)
            .map(|meta| Arc::clone(&meta.connector));
        tokio::spawn(async move {
            if let (Some(connector), Ok(addr)) = (connector, ip.parse()) {
                let _ = connector.probe_arp_cache(addr).await;
            }
            let mut map = serde_json::Map::new();
            if let Some(mac) = crate::arp::lookup_mac(&ip).await {
                map.insert("mac".into(), Value::String(mac));
            }
            reply_json(&query, &Value::Object(map)).await;
        });
    }

    /// The M2b decision made concrete: a bounded scan is one query whose
    /// findings stream back as multiple replies — dropping the query at the
    /// end finalizes it. Open-ended wizard scans stay session-pub-shaped
    /// (deferred until a wizard consumer exists).
    fn handle_discover(&self, query: Query, body: &Value) {
        let request: rpc::Discover = match parse(body) {
            Ok(request) => request,
            Err(error) => {
                tokio::spawn(async move { reply_err(&query, &error).await });
                return;
            }
        };
        // Clamp under the client-side query timeout so replies land in time.
        let window = Duration::from_millis(request.timeout_ms.unwrap_or(3000).min(8000));
        let discoveries = Arc::clone(&self.discoveries);
        tokio::spawn(async move {
            if let Some(vendor) = &request.vendor
                && !discoveries.contains_key(vendor)
            {
                reply_err(&query, &WireError::unknown_vendor(vendor)).await;
                return;
            }
            for (vendor, discovery) in discoveries.iter() {
                if request.vendor.as_deref().is_some_and(|v| v != vendor) {
                    continue;
                }
                match discovery.discover(window).await {
                    Ok(found) => {
                        for mut tv in found {
                            // The scan itself primed the ARP cache — attach
                            // the MAC so pairing configs built from this
                            // finding are WoL-capable.
                            if !tv.vendor_data.contains_key("mac")
                                && let Some(mac) = crate::arp::lookup_mac(&tv.address).await
                            {
                                tv.vendor_data.insert("mac".into(), mac);
                            }
                            let Ok(bytes) = serde_json::to_vec(&tv) else {
                                continue;
                            };
                            let key = query.key_expr().clone().into_owned();
                            if let Err(e) = query
                                .reply(key, bytes)
                                .encoding(Encoding::APPLICATION_JSON)
                                .await
                            {
                                warn!(vendor, %e, "discovery reply failed");
                            }
                        }
                    }
                    Err(error) => warn!(vendor, %error, "discovery scan failed"),
                }
            }
        });
    }

    /// Validate a vendor-config object against the vendor's `UserInput`
    /// schema (the same projection the wizard and edit pane render).
    /// Full-config mode (`partial = false`, add-tv/pair) requires every
    /// required field present and non-blank; patch mode (`partial =
    /// true`, update-config) only judges keys the patch touches — a
    /// required field set to blank or `null` (the unset request) is
    /// rejected. Type parsing stays vendor-side; this catches the
    /// field-shaped mistakes a client can route back to a form row.
    /// Unknown vendors skip validation — the world reports those.
    fn validate_vendor_fields(
        &self,
        vendor: &str,
        config: &Value,
        partial: bool,
    ) -> Result<(), WireError> {
        let Some(meta) = self.catalog.iter().find(|m| m.id == vendor) else {
            return Ok(());
        };
        let Some(object) = config.as_object() else {
            return Ok(());
        };
        for field in meta.connector.pairing_schema() {
            if !field.required {
                continue;
            }
            let value = object.get(&field.name);
            let blank = match value {
                Some(Value::String(s)) => s.trim().is_empty(),
                Some(Value::Null) => true,
                Some(_) => false,
                None => !partial,
            };
            if blank {
                return Err(WireError::invalid_field(&field.name, "required"));
            }
        }
        Ok(())
    }

    /// Map an RPC key + JSON body onto a world command. The reply
    /// correlation id is threaded through so the world's reply comes back
    /// to this query.
    async fn route(
        &self,
        key: &str,
        body: &Value,
        reply: ReplyTo,
    ) -> Result<(Command, Option<Value>), WireError> {
        if let Some(verb) = keys::parse_manager_rpc(&self.site, key) {
            return match verb {
                "add-tv" => {
                    let req: rpc::AddTv = parse(body)?;
                    let id = req.id.unwrap_or_else(Uuid::new_v4);
                    let config = enrich_config_mac(req.config).await;
                    self.validate_vendor_fields(&req.vendor, &config, false)?;
                    Ok((
                        Command::AddTv {
                            record: Box::new(tisvlay_world::TvRecord {
                                id,
                                name: req.name,
                                vendor: req.vendor,
                                managed: req.managed,
                                system_profile: None,
                                primary_input: None,
                                automate_when: "always".into(),
                                config: json_to_toml(&config)?,
                            }),
                            reply: Some(reply),
                        },
                        Some(json!({ "id": id })),
                    ))
                }
                "pair" => {
                    let req: rpc::AddTv = parse(body)?;
                    let id = req.id.unwrap_or_else(Uuid::new_v4);
                    let config = enrich_config_mac(req.config).await;
                    self.validate_vendor_fields(&req.vendor, &config, false)?;
                    Ok((
                        Command::Pair {
                            id,
                            name: req.name,
                            vendor: req.vendor,
                            config: json_to_toml(&config)?,
                            managed: req.managed,
                            reply: Some(reply),
                        },
                        Some(json!({ "id": id })),
                    ))
                }
                "cancel-pair" => {
                    let req: rpc::TvRef = parse(body)?;
                    Ok((
                        Command::CancelPair {
                            id: req.id,
                            reply: Some(reply),
                        },
                        None,
                    ))
                }
                _ => Err(unknown_rpc(verb)),
            };
        }
        if let Some(verb) = keys::parse_profile_rpc(&self.site, key) {
            return match verb {
                "create" => {
                    let req: rpc::ProfileCreate = parse(body)?;
                    let id = req.id.unwrap_or_else(Uuid::new_v4);
                    Ok((
                        Command::AddProfile {
                            record: Box::new(tisvlay_world::ProfileRecord { id, body: req.body }),
                            reply: Some(reply),
                        },
                        Some(json!({ "id": id })),
                    ))
                }
                "update" => {
                    let req: rpc::ProfileUpdate = parse(body)?;
                    Ok((
                        Command::UpdateProfile {
                            id: req.id,
                            body: Box::new(req.body),
                            reply: Some(reply),
                        },
                        None,
                    ))
                }
                "duplicate" => {
                    let req: rpc::ProfileDuplicate = parse(body)?;
                    let new_id = Uuid::new_v4();
                    Ok((
                        Command::DuplicateProfile {
                            id: req.id,
                            new_id,
                            name: req.name,
                            reply: Some(reply),
                        },
                        Some(json!({ "id": new_id })),
                    ))
                }
                "delete" => {
                    let req: rpc::TvRef = parse(body)?;
                    Ok((
                        Command::DeleteProfile {
                            id: req.id,
                            reply: Some(reply),
                        },
                        None,
                    ))
                }
                "defaults" => Ok((Command::ProfileDefaults { reply: Some(reply) }, None)),
                "effective" => {
                    let req: rpc::TvRef = parse(body)?;
                    Ok((
                        Command::EffectiveProfile {
                            id: req.id,
                            reply: Some(reply),
                        },
                        None,
                    ))
                }
                _ => Err(unknown_rpc(verb)),
            };
        }
        if let Some((id, verb)) = keys::parse_tv_rpc(&self.site, key) {
            let command = match verb {
                "set-volume" => {
                    let req: rpc::SetVolume = parse(body)?;
                    Command::SetVolume {
                        id,
                        level: req.level,
                        reply: Some(reply),
                    }
                }
                "set-muted" => {
                    let req: rpc::SetMuted = parse(body)?;
                    Command::SetMuted {
                        id,
                        muted: req.muted,
                        reply: Some(reply),
                    }
                }
                "set-name" => {
                    let req: rpc::SetName = parse(body)?;
                    Command::SetName {
                        id,
                        name: req.name,
                        reply: Some(reply),
                    }
                }
                "set-managed" => {
                    let req: rpc::SetManaged = parse(body)?;
                    Command::SetManaged {
                        id,
                        managed: req.managed,
                        reply: Some(reply),
                    }
                }
                "set-system-profile" => {
                    let req: rpc::SetSystemProfile = parse(body)?;
                    Command::SetSystemProfile {
                        id,
                        profile: req.profile,
                        reply: Some(reply),
                    }
                }
                "set-primary-input" => {
                    let req: rpc::SetPrimaryInput = parse(body)?;
                    Command::SetPrimaryInput {
                        id,
                        input: req.input,
                        reply: Some(reply),
                    }
                }
                "set-automate-when" => {
                    let req: rpc::SetAutomateWhen = parse(body)?;
                    // Validate against the enum here so the world only
                    // ever stores gate values `is_automated` understands.
                    serde_json::from_value::<tisvlay_core::AutomateWhen>(Value::String(
                        req.when.clone(),
                    ))
                    .map_err(|_| bad_request(format!("unknown automate_when '{}'", req.when)))?;
                    Command::SetAutomateWhen {
                        id,
                        when: req.when,
                        reply: Some(reply),
                    }
                }
                "switch-input" => {
                    let req: rpc::SwitchInput = parse(body)?;
                    Command::SwitchInput {
                        id,
                        input: req.input,
                        reply: Some(reply),
                    }
                }
                "set-backlight" => {
                    let req: rpc::SetBacklight = parse(body)?;
                    Command::SetBacklight {
                        id,
                        level: req.level,
                        reply: Some(reply),
                    }
                }
                "set-screen" => {
                    let req: rpc::SetScreen = parse(body)?;
                    Command::SetScreen {
                        id,
                        on: req.on,
                        reply: Some(reply),
                    }
                }
                "get-config" => Command::GetConfig {
                    id,
                    reply: Some(reply),
                },
                "update-config" => {
                    if !body.is_object() {
                        return Err(bad_request("config patch must be a JSON object".into()));
                    }
                    if let Some(vendor) = self.vendors_by_tv.get(&id) {
                        self.validate_vendor_fields(&vendor.clone(), body, true)?;
                    }
                    Command::UpdateConfig {
                        id,
                        patch: body.clone(),
                        reply: Some(reply),
                    }
                }
                "re-pair" => Command::RePair {
                    id,
                    reply: Some(reply),
                },
                "power-on" => Command::PowerOn {
                    id,
                    reply: Some(reply),
                },
                "power-off" => Command::PowerOff {
                    id,
                    reply: Some(reply),
                },
                "reconnect" => Command::Reconnect {
                    id,
                    reply: Some(reply),
                },
                "remove" => Command::RemoveTv {
                    id,
                    reply: Some(reply),
                },
                _ => return Err(unknown_rpc(verb)),
            };
            return Ok((command, None));
        }
        Err(unknown_rpc(key))
    }
}

async fn reply_json(query: &Query, payload: &Value) {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    let key = query.key_expr().clone().into_owned();
    if let Err(e) = query
        .reply(key, bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        warn!(%e, "reply failed");
    }
}

async fn reply_err(query: &Query, error: &WireError) {
    let bytes = serde_json::to_vec(error).unwrap_or_default();
    if let Err(e) = query
        .reply_err(bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        warn!(%e, "reply_err failed");
    }
}

fn parse<T: DeserializeOwned>(body: &Value) -> Result<T, WireError> {
    serde_json::from_value(body.clone()).map_err(|e| bad_request(e.to_string()))
}

fn bad_request(message: String) -> WireError {
    WireError {
        code: "bad-request".into(),
        message,
    }
}

fn unknown_rpc(what: &str) -> WireError {
    WireError {
        code: "unknown-rpc".into(),
        message: format!("no such rpc: {what}"),
    }
}

/// Vendor configs are JSON on the wire and `toml::Value` at the backend
/// seam — a pure serde transcode (JSON `null` has no TOML form and errors).
fn json_to_toml(value: &Value) -> Result<toml::Value, WireError> {
    toml::Value::try_from(value).map_err(|e| bad_request(format!("vendor config: {e}")))
}

/// An IP-addressed vendor config without a `mac` can't Wake-on-LAN. The
/// pairing/connect traffic primes the ARP cache, so fill it in here — an
/// explicit `mac` from the caller always wins.
async fn enrich_config_mac(mut config: Value) -> Value {
    if let Some(object) = config.as_object_mut()
        && !object.contains_key("mac")
        && let Some(ip) = object.get("ip").and_then(Value::as_str)
        && let Some(mac) = crate::arp::lookup_mac(ip).await
    {
        object.insert("mac".into(), Value::String(mac));
    }
    config
}
