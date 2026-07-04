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
use tokio::sync::mpsc;
use tracing::{debug, warn};
use tvnext_wire::{keys, rpc};
use tvnext_world::{Command, ReplyTo, WireEffect, WireError, WorldClient};
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

pub(crate) async fn run(
    session: Session,
    site: String,
    mut effects: mpsc::UnboundedReceiver<WireEffect>,
    world: WorldClient,
    discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>>,
) -> anyhow::Result<()> {
    let (query_tx, mut queries) = mpsc::unbounded_channel::<Query>();
    let mut queryables: Vec<Queryable<()>> = Vec::new();
    for key in [keys::manager_rpc_wild(&site), keys::tv_rpc_wild(&site)] {
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

    let mut bridge = Bridge {
        session,
        site,
        world,
        discoveries,
        publishers: HashMap::new(),
        pending: HashMap::new(),
        next_reply: 1,
        pairing_tokens: HashMap::new(),
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
    publishers: HashMap<String, AdvancedPublisher<'static>>,
    pending: HashMap<ReplyTo, PendingQuery>,
    next_reply: ReplyTo,
    pairing_tokens: HashMap<String, LivelinessToken>,
}

impl Bridge {
    async fn handle_effect(&mut self, effect: WireEffect) {
        match effect {
            WireEffect::Publish { key, payload } => {
                self.update_pairing_liveliness(&key, &payload).await;
                self.publish(&key, &payload).await;
            }
            WireEffect::Delete { key } => match self.publishers.remove(&key) {
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
            },
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
        // Discovery is a stateless read — served bridge-side with streamed
        // replies, never touching the world.
        if keys::parse_manager_rpc(&self.site, &key) == Some("discover") {
            self.handle_discover(query, &body);
            return;
        }
        let reply_id = self.next_reply;
        self.next_reply += 1;
        match self.route(&key, &body, reply_id) {
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
                                && let Some(mac) = crate::arp::lookup_mac(&tv.address)
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

    /// Map an RPC key + JSON body onto a world command. The reply
    /// correlation id is threaded through so the world's reply comes back
    /// to this query.
    fn route(
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
                    let config = enrich_config_mac(req.config);
                    Ok((
                        Command::AddTv {
                            record: Box::new(tvnext_world::TvRecord {
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
                    let config = enrich_config_mac(req.config);
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
fn enrich_config_mac(mut config: Value) -> Value {
    if let Some(object) = config.as_object_mut()
        && !object.contains_key("mac")
        && let Some(ip) = object.get("ip").and_then(Value::as_str)
        && let Some(mac) = crate::arp::lookup_mac(ip)
    {
        object.insert("mac".into(), Value::String(mac));
    }
    config
}
