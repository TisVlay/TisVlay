//! Zenoh ↔ world bridge: wire effects out (cached publishes, deletes,
//! query replies), RPC queries in (world commands with reply correlation).
//!
//! One task owns everything — the publisher map, the pending-reply map,
//! and the pairing liveliness tokens — so there are no locks here either.
//! Each rpc family decodes into its endpoint `Call` enum and is matched
//! exhaustively: a new rpc without a route here doesn't compile.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tisvlay_core::VendorConfig;
use tisvlay_wire::endpoint::{manager, profile, tv, vendors};
use tisvlay_wire::{keys, rpc, serve};
use tisvlay_world::{Command, Reply, ReplyTo, WireEffect, WireError, WorldClient};
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

#[expect(
    clippy::too_many_arguments,
    reason = "an entry point taking each handle and channel it owns; a struct would only rename the list"
)]
pub(crate) async fn run(
    session: Session,
    site: String,
    mut effects: mpsc::UnboundedReceiver<WireEffect>,
    world: WorldClient,
    discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>>,
    catalog: Arc<Vec<VendorMeta>>,
    info: rpc::ServiceInfo,
    ready: tokio::sync::oneshot::Sender<()>,
) -> anyhow::Result<()> {
    let (query_tx, mut queries) = mpsc::unbounded_channel::<Query>();
    let mut queryables: Vec<Queryable<()>> = Vec::new();
    // `vendors/**` also matches the bare catalog key: `**` spans zero
    // chunks too.
    for key in [
        keys::manager_rpc_wild(&site),
        keys::tv_rpc_wild(&site),
        keys::profile_rpc_wild(&site),
        keys::vendors_wild(&site),
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

struct Bridge {
    session: Session,
    site: String,
    world: WorldClient,
    discoveries: Arc<HashMap<String, Arc<dyn DynDiscovery>>>,
    catalog: Arc<Vec<VendorMeta>>,
    info: rpc::ServiceInfo,
    publishers: HashMap<String, AdvancedPublisher<'static>>,
    /// Queries the world owes a reply, by correlation id.
    pending: HashMap<ReplyTo, Query>,
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
                let Some(query) = self.pending.remove(&to) else {
                    warn!(to, "reply for unknown query dropped");
                    return;
                };
                match result {
                    Ok(value) => serve::reply_json(&query, &value).await,
                    Err(error) => serve::reply_err(&query, &error).await,
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
        let payload = serve::payload(&query);
        let reply = self.next_reply;
        let command = if key.starts_with(&keys::vendors(&self.site)) {
            match vendors::Call::decode(&self.site, &key, &payload) {
                Ok(call) => {
                    self.serve_vendors(&query, call).await;
                    return;
                }
                Err(error) => Err(error),
            }
        } else if let Some(verb) = keys::parse_manager_rpc(&self.site, &key) {
            match manager::Call::decode(verb, &payload) {
                Err(error) => Err(error),
                // Introspection and discovery are served bridge-side,
                // never touching the world.
                Ok(manager::Call::Info(())) => {
                    serve::reply::<manager::Info>(&query, &self.info).await;
                    return;
                }
                Ok(manager::Call::Enrich(request)) => {
                    self.handle_enrich(query, request);
                    return;
                }
                Ok(manager::Call::Discover(request)) => {
                    self.handle_discover(query, request);
                    return;
                }
                Ok(manager::Call::DiscoverStart(request)) => {
                    self.handle_discover_start(&query, request).await;
                    return;
                }
                Ok(manager::Call::DiscoverStop(request)) => {
                    self.handle_discover_stop(&query, request).await;
                    return;
                }
                Ok(manager::Call::AddTv(request)) => self.add_tv(request, reply).await,
                Ok(manager::Call::Pair(request)) => self.pair(request, reply).await,
                Ok(manager::Call::CancelPair(request)) => Ok(Command::CancelPair {
                    id: request.id,
                    reply: Some(Reply::new(reply)),
                }),
            }
        } else if let Some(verb) = keys::parse_profile_rpc(&self.site, &key) {
            profile::Call::decode(verb, &payload).map(|call| profile_command(call, reply))
        } else if let Some((id, verb)) = keys::parse_tv_rpc(&self.site, &key) {
            tv::Call::decode(verb, &payload).and_then(|call| self.tv_command(id, call, reply))
        } else {
            Err(WireError::unknown_rpc(&key))
        };
        match command {
            Ok(command) => {
                debug!(%key, reply, "rpc routed");
                self.next_reply += 1;
                self.pending.insert(reply, query);
                if !self.world.command(command)
                    && let Some(query) = self.pending.remove(&reply)
                {
                    serve::reply_err(&query, &WireError::daemon_shutting_down()).await;
                }
            }
            Err(error) => serve::reply_err(&query, &error).await,
        }
    }

    /// The vendor catalog and per-vendor pairing schemas.
    async fn serve_vendors(&self, query: &Query, call: vendors::Call) {
        match call {
            vendors::Call::Catalog(()) => {
                let list: Vec<rpc::VendorEntry> = self
                    .catalog
                    .iter()
                    .map(|v| rpc::VendorEntry {
                        id: v.id.clone(),
                        name: v.name.clone(),
                        capabilities: v.capabilities.clone(),
                    })
                    .collect();
                serve::reply::<vendors::Catalog>(query, &list).await;
            }
            vendors::Call::Schema { vendor } => {
                match self.catalog.iter().find(|v| v.id == vendor) {
                    Some(meta) => {
                        serve::reply::<vendors::Schema>(query, &meta.connector.pairing_schema())
                            .await;
                    }
                    None => serve::reply_err(query, &WireError::unknown_vendor(&vendor)).await,
                }
            }
        }
    }

    /// Enroll a TV from a full config.
    async fn add_tv(&self, request: rpc::AddTv, reply: ReplyTo) -> Result<Command, WireError> {
        let id = request.id.unwrap_or_else(Uuid::new_v4);
        let config = self.prepare_config(&request.vendor, request.config).await?;
        Ok(Command::AddTv {
            record: Box::new(tisvlay_world::TvRecord {
                id,
                name: request.name,
                vendor: request.vendor,
                managed: request.managed,
                system_profile: None,
                primary_input: None,
                config,
            }),
            reply: Some(Reply::new(reply)),
        })
    }

    /// Start pairing a new TV.
    async fn pair(&self, request: rpc::AddTv, reply: ReplyTo) -> Result<Command, WireError> {
        let config = self.prepare_config(&request.vendor, request.config).await?;
        Ok(Command::Pair {
            id: request.id.unwrap_or_else(Uuid::new_v4),
            attempt: Uuid::new_v4(),
            name: request.name,
            vendor: request.vendor,
            config,
            managed: request.managed,
            reply: Some(Reply::new(reply)),
        })
    }

    /// Map a per-TV rpc onto its world command.
    fn tv_command(&self, id: Uuid, call: tv::Call, reply: ReplyTo) -> Result<Command, WireError> {
        Ok(match call {
            tv::Call::SetVolume(request) => Command::SetVolume {
                id,
                level: request.level,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetMuted(request) => Command::SetMuted {
                id,
                muted: request.muted,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetName(request) => Command::SetName {
                id,
                name: request.name,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetManaged(request) => Command::SetManaged {
                id,
                managed: request.managed,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetSystemProfile(request) => Command::SetSystemProfile {
                id,
                profile: request.profile,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetPrimaryInput(request) => Command::SetPrimaryInput {
                id,
                input: request.input,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SwitchInput(request) => Command::SwitchInput {
                id,
                input: request.input,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetBacklight(request) => Command::SetBacklight {
                id,
                level: request.level,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::SetScreen(request) => Command::SetScreen {
                id,
                on: request.on,
                restore_input: request.restore_input,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::PowerOn(request) => Command::PowerOn {
                id,
                restore_input: request.restore_input,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::PowerOff(()) => Command::PowerOff {
                id,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::Reconnect(()) => Command::Reconnect {
                id,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::Remove(()) => Command::RemoveTv {
                id,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::GetConfig(()) => Command::GetConfig {
                id,
                reply: Some(Reply::new(reply)),
            },
            tv::Call::UpdateConfig(patch) => {
                if let Some(vendor) = self.vendors_by_tv.get(&id) {
                    self.validate_vendor_fields(vendor, &patch.0, true)?;
                }
                Command::UpdateConfig {
                    id,
                    patch,
                    reply: Some(Reply::new(reply)),
                }
            }
            tv::Call::RePair(()) => Command::RePair {
                id,
                attempt: Uuid::new_v4(),
                reply: Some(Reply::new(reply)),
            },
        })
    }

    /// Open-ended scan session: publish findings until stopped, with a
    /// liveliness token marking the session alive (dropped tokens tell
    /// wizards the daemon or session died).
    async fn handle_discover_start(&mut self, query: &Query, request: rpc::DiscoverStart) {
        if let Some(vendor) = &request.vendor
            && !self.discoveries.contains_key(vendor)
        {
            serve::reply_err(query, &WireError::unknown_vendor(vendor)).await;
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
                                if !tv.vendor_data.contains_key(tisvlay_core::MAC_CONFIG_KEY)
                                    && let Some(mac) = crate::arp::lookup_mac(&tv.address).await
                                {
                                    tv.vendor_data
                                        .insert(tisvlay_core::MAC_CONFIG_KEY.into(), mac);
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
        serve::reply::<manager::DiscoverStart>(
            query,
            &rpc::DiscoverySession {
                session: session_id,
            },
        )
        .await;
    }

    async fn handle_discover_stop(&mut self, query: &Query, request: rpc::DiscoverStop) {
        match self.discovery_sessions.remove(&request.session) {
            Some(handle) => {
                // Aborting drops the task's liveliness token with it.
                handle.abort();
                serve::reply::<manager::DiscoverStop>(query, &rpc::Ack {}).await;
            }
            None => {
                serve::reply_err(query, &WireError::discovery_not_found(request.session)).await;
            }
        }
    }

    /// Manual-entry enrichment: probe the ARP cache for `ip` (after the
    /// vendor primes it) so the wizard can hint the device's MAC before
    /// pairing. Best-effort — an empty reply just means no hint.
    fn handle_enrich(&self, query: Query, request: rpc::Enrich) {
        let connector = self
            .catalog
            .iter()
            .find(|meta| meta.id == request.vendor)
            .map(|meta| Arc::clone(&meta.connector));
        tokio::spawn(async move {
            if let (Some(connector), Ok(addr)) = (connector, request.ip.parse()) {
                let _ = connector.probe_arp_cache(addr).await;
            }
            let reply = rpc::EnrichReply {
                mac: crate::arp::lookup_mac(&request.ip).await,
            };
            serve::reply::<manager::Enrich>(&query, &reply).await;
        });
    }

    /// A bounded scan is one query whose findings stream back as multiple
    /// replies; dropping the query at the end finalizes it.
    fn handle_discover(&self, query: Query, request: rpc::Discover) {
        let window = manager::discover_scan(&request);
        let discoveries = Arc::clone(&self.discoveries);
        tokio::spawn(async move {
            if let Some(vendor) = &request.vendor
                && !discoveries.contains_key(vendor)
            {
                serve::reply_err(&query, &WireError::unknown_vendor(vendor)).await;
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
                            if !tv.vendor_data.contains_key(tisvlay_core::MAC_CONFIG_KEY)
                                && let Some(mac) = crate::arp::lookup_mac(&tv.address).await
                            {
                                tv.vendor_data
                                    .insert(tisvlay_core::MAC_CONFIG_KEY.into(), mac);
                            }
                            serve::reply::<manager::Discover>(&query, &tv).await;
                        }
                    }
                    Err(error) => warn!(vendor, %error, "discovery scan failed"),
                }
            }
        });
    }

    /// An enrolling TV's vendor config: the MAC filled from the ARP cache
    /// when absent, the vendor's required fields checked, then converted
    /// for the backend.
    async fn prepare_config(
        &self,
        vendor: &str,
        mut config: serde_json::Map<String, Value>,
    ) -> Result<VendorConfig, WireError> {
        enrich_config_mac(&mut config).await;
        self.validate_vendor_fields(vendor, &config, false)?;
        vendor_config_from_json(&config)
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
        object: &serde_json::Map<String, Value>,
        partial: bool,
    ) -> Result<(), WireError> {
        let Some(meta) = self.catalog.iter().find(|m| m.id == vendor) else {
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
}

/// Map a system-profile rpc onto its world command.
fn profile_command(call: profile::Call, reply: ReplyTo) -> Command {
    match call {
        profile::Call::Create(request) => Command::AddProfile {
            record: Box::new(tisvlay_world::ProfileRecord {
                id: request.id.unwrap_or_else(Uuid::new_v4),
                body: request.body,
            }),
            reply: Some(Reply::new(reply)),
        },
        profile::Call::Update(request) => Command::UpdateProfile {
            id: request.id,
            body: Box::new(request.body),
            reply: Some(Reply::new(reply)),
        },
        profile::Call::Duplicate(request) => Command::DuplicateProfile {
            id: request.id,
            new_id: Uuid::new_v4(),
            name: request.name,
            reply: Some(Reply::new(reply)),
        },
        profile::Call::Delete(request) => Command::DeleteProfile {
            id: request.id,
            reply: Some(Reply::new(reply)),
        },
        profile::Call::Defaults(()) => Command::ProfileDefaults {
            reply: Some(Reply::new(reply)),
        },
        profile::Call::Effective(request) => Command::EffectiveProfile {
            id: request.id,
            reply: Some(Reply::new(reply)),
        },
    }
}

/// Vendor configs are JSON on the wire and a [`VendorConfig`] at the backend
/// seam — a pure serde transcode (a JSON `null` has no TOML form and
/// errors).
fn vendor_config_from_json(
    config: &serde_json::Map<String, Value>,
) -> Result<VendorConfig, WireError> {
    VendorConfig::from_typed(config)
        .map_err(|e| WireError::bad_request(format!("vendor config: {e}")))
}

/// An IP-addressed vendor config without a `mac` can't Wake-on-LAN. The
/// pairing/connect traffic primes the ARP cache, so fill it in here — an
/// explicit `mac` from the caller always wins.
async fn enrich_config_mac(config: &mut serde_json::Map<String, Value>) {
    if !config.contains_key(tisvlay_core::MAC_CONFIG_KEY)
        && let Some(ip) = config.get("ip").and_then(Value::as_str)
        && let Some(mac) = crate::arp::lookup_mac(ip).await
    {
        config.insert(tisvlay_core::MAC_CONFIG_KEY.into(), Value::String(mac));
    }
}
