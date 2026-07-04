//! GUI-facing client SDK: the root stack's `DaemonClient` /
//! `SessionClient` / `TrayClient` / `StateWatcher` method surface,
//! reimplemented over zenoh so the forked GUI ports with import swaps.
//!
//! Reads come from client-side mirrors (history subscribers over the
//! cached keys — the wire's `GetManagedObjects`); writes are rpcs;
//! change signals are the mirrors' own deltas. TV ids stay strings at
//! this boundary, matching the root client the GUI was written against.

mod mirror;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tisvlay_core::{
    Capabilities, DesktopMapping, DiscoveredTv, HardwareId, PairingField, SystemProfileConfig,
    UserProfileConfig,
};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;
use tvnext_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use mirror::{SystemMirror, UserMirror};

/// Body aliases: the wire carries the config structs verbatim (name +
/// per-field options), so the D-Bus-era body wrappers collapse.
pub type SystemProfileBody = SystemProfileConfig;
pub type UserProfileBody = UserProfileConfig;

/// Pairing progress detail — field-compatible with the root core type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingProgressDetail {
    pub message: String,
    pub code: Option<String>,
    pub path: Option<String>,
}

/// Terminal pairing outcome, drained from the progress key.
#[derive(Debug, Clone)]
pub enum PairTerminal {
    Complete {
        tv_id: String,
        message: String,
    },
    Failed {
        tv_id: String,
        detail: PairingProgressDetail,
    },
}

/// One pairing-progress sample with its TV correlation.
#[derive(Debug, Clone)]
pub struct PairingEvent {
    pub tv_id: String,
    pub phase: String,
    pub detail: PairingProgressDetail,
}

/// One power-cycle progress sample (`progress/power` key).
#[derive(Debug, Clone)]
pub struct PowerCycleEvent {
    pub tv_id: String,
    pub phase: String,
}

/// The first key in a change window — root parity for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvStateChange {
    pub key: String,
}

/// Change notifications the GUI's refresh dispatcher consumes. Variant
/// set mirrors the root watcher; the tray ones exist so match arms port
/// verbatim (they never fire — the spike tray has no settings service).
#[derive(Debug, Clone)]
pub enum StateEvent {
    DaemonStateChanged(TvStateChange),
    DaemonAppeared,
    DaemonDisappeared,
    SessionAppeared,
    SessionDisappeared,
    TvUserProfileChanged,
    SystemProfileChanged(Uuid),
    SystemProfileAdded(Uuid),
    SystemProfileRemoved(Uuid),
    UserProfileChanged(Uuid),
    UserProfileAdded(Uuid),
    UserProfileRemoved(Uuid),
    TrayAppeared,
    TrayDisappeared,
    TrayNotificationSettingsChanged,
    FallbackPoll,
}

/// Root-compatible notification settings body (the spike tray has no
/// settings service; the type exists so the settings pane wiring ports).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationSettingsBody {
    pub tv_connection: Option<bool>,
    pub tv_pairing: Option<bool>,
    pub tv_wake: Option<bool>,
    pub daemon_offline: Option<bool>,
}

/// Everything `list_tvs` reports per TV — field set matches the root
/// `TvInfo` so `state.rs` ports unchanged.
#[derive(Debug, Clone, Default)]
pub struct TvInfo {
    pub uuid: String,
    pub name: String,
    pub vendor: String,
    pub brand: String,
    pub capabilities: Capabilities,
    pub connected: bool,
    pub managed: bool,
    pub automated: bool,
    pub system_profile: String,
    pub primary_input: String,
    pub hardware_id: HardwareId,
    pub vendor_config: Vec<(String, String)>,
    pub vendor_info: Vec<(String, String)>,
    pub desktop: DesktopMapping,
    pub path: String,
    pub power_state: String,
    pub supports_wake: bool,
    pub is_screen_on: bool,
    pub volume: u8,
    pub muted: bool,
    pub current_input: String,
    pub backlight: u8,
    pub inputs: Vec<(String, String)>,
}

const EVENT_CAPACITY: usize = 256;
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Client for the system daemon: mirror reads, rpc writes.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    session: Session,
    site: String,
    mirror: Arc<Mutex<SystemMirror>>,
    events: broadcast::Sender<StateEvent>,
}

impl DaemonClient {
    /// Connect to the daemon socket and start mirroring.
    pub async fn connect(socket: &Path, site: &str) -> Result<Self> {
        let session = zenoh::open(bootstrap::client_config(socket)?)
            .await
            .map_err(|e| anyhow!("connect to daemon socket {}: {e}", socket.display()))?;
        let mirror = Arc::new(Mutex::new(SystemMirror::default()));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);

        for key in [keys::tv_all(site), keys::profile_system_wild(site)] {
            let sub = session
                .declare_subscriber(key.as_str())
                .history(HistoryConfig::default().detect_late_publishers())
                .await
                .map_err(|e| anyhow!("subscribe {key}: {e}"))?;
            let mirror = Arc::clone(&mirror);
            let events = events.clone();
            let site = site.to_string();
            tokio::spawn(async move {
                while let Ok(sample) = sub.recv_async().await {
                    let event = mirror.lock().expect("mirror lock").apply(&site, &sample);
                    if let Some(event) = event {
                        let _ = events.send(event);
                    }
                }
            });
        }
        let liveliness = session
            .liveliness()
            .declare_subscriber(keys::daemon_alive(site))
            .history(true)
            .await
            .map_err(|e| anyhow!("subscribe daemon liveliness: {e}"))?;
        {
            let mirror = Arc::clone(&mirror);
            let events = events.clone();
            tokio::spawn(async move {
                while let Ok(sample) = liveliness.recv_async().await {
                    let alive = sample.kind() == SampleKind::Put;
                    mirror.lock().expect("mirror lock").daemon_alive = alive;
                    let _ = events.send(if alive {
                        StateEvent::DaemonAppeared
                    } else {
                        StateEvent::DaemonDisappeared
                    });
                }
            });
        }
        Ok(Self {
            session,
            site: site.to_string(),
            mirror,
            events,
        })
    }

    pub async fn is_available(&self) -> bool {
        self.mirror.lock().expect("mirror lock").daemon_alive
    }

    /// Assemble the fleet snapshot the GUI renders from. Vendor config
    /// rows come from a per-TV `get-config` rpc (small fleets).
    pub async fn list_tvs(&self) -> Result<Vec<TvInfo>> {
        let rows: Vec<(Uuid, mirror_row::Row)> = {
            let mirror = self.mirror.lock().expect("mirror lock");
            mirror
                .tvs
                .iter()
                .map(|(id, row)| (*id, mirror_row::Row::from(row)))
                .collect()
        };
        let mut tvs = Vec::with_capacity(rows.len());
        for (id, row) in rows {
            let vendor_config = match self.get_tv_config(&id.to_string()).await {
                Ok(config) => flatten_config(&config),
                Err(_) => Vec::new(),
            };
            tvs.push(row.into_tv_info(id, &self.site, vendor_config));
        }
        Ok(tvs)
    }

    pub async fn version(&self) -> Result<String> {
        Ok(format!("{} (zenoh spike)", env!("CARGO_PKG_VERSION")))
    }

    pub async fn config_path(&self) -> Result<String> {
        Ok(String::new())
    }

    pub async fn reload_config(&self) -> Result<()> {
        // The zenoh daemon has no reloadable config file; the mirror is
        // already live.
        Ok(())
    }

    pub async fn get_supported_vendors(&self) -> Result<Vec<(String, String)>> {
        let list = self.call(&keys::vendors(&self.site), Value::Null).await?;
        Ok(list
            .as_array()
            .map(|vendors| {
                vendors
                    .iter()
                    .filter_map(|vendor| {
                        Some((
                            vendor["id"].as_str()?.to_string(),
                            vendor["name"].as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn get_pairing_schema(&self, vendor: &str) -> Result<Vec<PairingField>> {
        let schema = self
            .call(&keys::vendor_schema(&self.site, vendor), Value::Null)
            .await?;
        serde_json::from_value(schema).context("decode pairing schema")
    }

    pub async fn pair_tv(&self, name: &str, vendor: &str, config: Value) -> Result<String> {
        let body = serde_json::to_value(rpc::AddTv {
            id: None,
            name: name.into(),
            vendor: vendor.into(),
            managed: true,
            config,
        })?;
        let ok = self
            .call(&keys::manager_rpc(&self.site, "pair"), body)
            .await?;
        ok["id"]
            .as_str()
            .map(str::to_string)
            .context("pair reply carries the id")
    }

    pub async fn re_pair(&self, tv: &str) -> Result<()> {
        let id = parse_tv(tv)?;
        self.call(&keys::tv_rpc(&self.site, &id, "re-pair"), Value::Null)
            .await
            .map(|_| ())
    }

    pub async fn cancel_pair(&self, tv: &str) -> Result<()> {
        let id = parse_tv(tv)?;
        self.call(
            &keys::manager_rpc(&self.site, "cancel-pair"),
            json!({ "id": id }),
        )
        .await
        .map(|_| ())
    }

    /// Live pairing-progress samples for one TV, hydrated with the
    /// session's history (the deep pairing cache replays prior phases).
    pub async fn pairing_progress_stream(
        &self,
        tv: &str,
    ) -> Result<mpsc::UnboundedReceiver<PairingEvent>> {
        let id = parse_tv(tv)?;
        let sub = self
            .session
            .declare_subscriber(keys::pairing_progress(&self.site, &id))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe pairing progress: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let tv = tv.to_string();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Ok(progress) = serde_json::from_slice::<Value>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                let event = PairingEvent {
                    tv_id: tv.clone(),
                    phase: progress["phase"].as_str().unwrap_or_default().to_string(),
                    detail: PairingProgressDetail {
                        message: progress["message"].as_str().unwrap_or_default().to_string(),
                        code: progress["code"].as_str().map(str::to_string),
                        path: progress["path"].as_str().map(str::to_string),
                    },
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    }

    /// `progress/power` samples for one TV (wake feedback).
    pub async fn power_cycle_progress_stream(
        &self,
        tv: &str,
    ) -> Result<mpsc::UnboundedReceiver<PowerCycleEvent>> {
        let id = parse_tv(tv)?;
        let sub = self
            .session
            .declare_subscriber(keys::tv_path(&self.site, &id, "progress/power"))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe power progress: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let tv = tv.to_string();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Ok(progress) = serde_json::from_slice::<Value>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                let event = PowerCycleEvent {
                    tv_id: tv.clone(),
                    phase: progress["phase"].as_str().unwrap_or_default().to_string(),
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
        });
        Ok(rx)
    }

    /// Open-ended discovery session; returns its id for the found-stream
    /// subscription and the eventual stop.
    pub async fn start_discovery(&self, vendor: Option<&str>) -> Result<String> {
        let body = serde_json::to_value(rpc::DiscoverStart {
            vendor: vendor.map(str::to_string),
        })?;
        let ok = self
            .call(&keys::manager_rpc(&self.site, "discover-start"), body)
            .await?;
        ok["session"]
            .as_str()
            .map(str::to_string)
            .context("discover-start reply carries the session")
    }

    pub async fn stop_discovery(&self, session_id: &str) -> Result<()> {
        let session: Uuid = session_id.parse().context("discovery session id")?;
        self.call(
            &keys::manager_rpc(&self.site, "discover-stop"),
            json!({ "session": session }),
        )
        .await
        .map(|_| ())
    }

    pub async fn discovery_found_stream(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<DiscoveredTv>> {
        let session: Uuid = session_id.parse().context("discovery session id")?;
        let sub = self
            .session
            .declare_subscriber(format!(
                "tisvlay/{}/discovery/{}/found/*",
                self.site, session
            ))
            .await
            .map_err(|e| anyhow!("subscribe discovery findings: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                if let Ok(found) =
                    serde_json::from_slice::<DiscoveredTv>(&sample.payload().to_bytes())
                    && tx.send(found).is_err()
                {
                    return;
                }
            }
        });
        Ok(rx)
    }

    pub async fn power_on(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "power-on", Value::Null).await
    }

    pub async fn power_off(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "power-off", Value::Null).await
    }

    pub async fn screen_on(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "set-screen", json!({ "on": true })).await
    }

    pub async fn screen_off(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "set-screen", json!({ "on": false })).await
    }

    pub async fn switch_input(&self, tv: &str, input: &str) -> Result<()> {
        self.tv_call(tv, "switch-input", json!({ "input": input }))
            .await
    }

    pub async fn set_volume(&self, tv: &str, level: u8) -> Result<()> {
        self.tv_call(tv, "set-volume", json!({ "level": level }))
            .await
    }

    pub async fn set_muted(&self, tv: &str, muted: bool) -> Result<()> {
        self.tv_call(tv, "set-muted", json!({ "muted": muted }))
            .await
    }

    pub async fn set_backlight(&self, tv: &str, level: u8) -> Result<()> {
        self.tv_call(tv, "set-backlight", json!({ "level": level }))
            .await
    }

    pub async fn set_name(&self, tv: &str, name: &str) -> Result<()> {
        self.tv_call(tv, "set-name", json!({ "name": name })).await
    }

    pub async fn set_managed(&self, tv: &str, managed: bool) -> Result<()> {
        self.tv_call(tv, "set-managed", json!({ "managed": managed }))
            .await
    }

    pub async fn set_primary_input(&self, tv: &str, input: Option<&str>) -> Result<()> {
        self.tv_call(tv, "set-primary-input", json!({ "input": input }))
            .await
    }

    pub async fn set_automate_when(&self, tv: &str, when: &str) -> Result<()> {
        self.tv_call(tv, "set-automate-when", json!({ "when": when }))
            .await
    }

    pub async fn set_system_profile(&self, tv: &str, profile: Option<Uuid>) -> Result<()> {
        self.tv_call(tv, "set-system-profile", json!({ "profile": profile }))
            .await
    }

    pub async fn reconnect(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "reconnect", Value::Null).await
    }

    pub async fn remove_tv(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "remove", Value::Null).await
    }

    pub async fn get_tv_config(&self, tv: &str) -> Result<Value> {
        let id = parse_tv(tv)?;
        self.call(&keys::tv_rpc(&self.site, &id, "get-config"), Value::Null)
            .await
    }

    pub async fn update_tv_config(&self, tv: &str, patch: Value) -> Result<()> {
        let id = parse_tv(tv)?;
        self.call(&keys::tv_rpc(&self.site, &id, "update-config"), patch)
            .await
            .map(|_| ())
    }

    pub async fn get_system_profile(&self, profile: Uuid) -> Result<SystemProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
            .with_context(|| format!("no system profile {profile}"))
    }

    pub async fn list_system_profile_bodies(&self) -> Result<HashMap<Uuid, SystemProfileBody>> {
        Ok(self
            .mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .iter()
            .map(|(id, body)| (*id, body.clone()))
            .collect())
    }

    pub async fn create_system_profile(&self, body: &SystemProfileBody) -> Result<Uuid> {
        let ok = self
            .call(
                &keys::profile_rpc(&self.site, "create"),
                json!({ "body": body }),
            )
            .await?;
        ok["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .context("create reply carries the id")
    }

    pub async fn update_system_profile(
        &self,
        profile: Uuid,
        body: &SystemProfileBody,
    ) -> Result<()> {
        self.call(
            &keys::profile_rpc(&self.site, "update"),
            json!({ "id": profile, "body": body }),
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_system_profile(&self, profile: Uuid) -> Result<()> {
        self.call(
            &keys::profile_rpc(&self.site, "delete"),
            json!({ "id": profile }),
        )
        .await
        .map(|_| ())
    }

    pub async fn duplicate_system_profile(&self, profile: Uuid, name: &str) -> Result<Uuid> {
        let ok = self
            .call(
                &keys::profile_rpc(&self.site, "duplicate"),
                json!({ "id": profile, "name": name }),
            )
            .await?;
        ok["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .context("duplicate reply carries the id")
    }

    /// The stock mapping as system-profile field values (JSON map).
    pub async fn get_resolved_system_profile_defaults(&self) -> Result<Value> {
        self.call(&keys::profile_rpc(&self.site, "defaults"), Value::Null)
            .await
    }

    /// One TV's resolved mapping (assigned profile over stock).
    pub async fn get_effective_system_profile(&self, tv: &str) -> Result<Value> {
        let id = parse_tv(tv)?;
        self.call(
            &keys::profile_rpc(&self.site, "effective"),
            json!({ "id": id }),
        )
        .await
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn tv_call(&self, tv: &str, verb: &str, body: Value) -> Result<()> {
        let id = parse_tv(tv)?;
        self.call(&keys::tv_rpc(&self.site, &id, verb), body)
            .await
            .map(|_| ())
    }

    async fn call(&self, key: &str, body: Value) -> Result<Value> {
        call_rpc(&self.session, key, body).await
    }
}

/// Client for the per-user session service (user profiles).
#[derive(Debug, Clone)]
pub struct SessionClient {
    session: Session,
    site: String,
    mirror: Arc<Mutex<UserMirror>>,
    events: broadcast::Sender<StateEvent>,
}

impl SessionClient {
    pub async fn connect(socket: &Path, site: &str) -> Result<Self> {
        let session = zenoh::open(bootstrap::client_config(socket)?)
            .await
            .map_err(|e| anyhow!("connect to session socket {}: {e}", socket.display()))?;
        let mirror = Arc::new(Mutex::new(UserMirror::default()));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let sub = session
            .declare_subscriber(keys::user_all(site))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe user keys: {e}"))?;
        {
            let mirror = Arc::clone(&mirror);
            let events = events.clone();
            let site = site.to_string();
            tokio::spawn(async move {
                while let Ok(sample) = sub.recv_async().await {
                    let event = mirror.lock().expect("mirror lock").apply(&site, &sample);
                    if let Some(event) = event {
                        let _ = events.send(event);
                    }
                }
            });
        }
        let liveliness = session
            .liveliness()
            .declare_subscriber(keys::user_alive_session(site))
            .history(true)
            .await
            .map_err(|e| anyhow!("subscribe session liveliness: {e}"))?;
        {
            let mirror = Arc::clone(&mirror);
            let events = events.clone();
            tokio::spawn(async move {
                while let Ok(sample) = liveliness.recv_async().await {
                    let alive = sample.kind() == SampleKind::Put;
                    mirror.lock().expect("mirror lock").session_alive = alive;
                    let _ = events.send(if alive {
                        StateEvent::SessionAppeared
                    } else {
                        StateEvent::SessionDisappeared
                    });
                }
            });
        }
        Ok(Self {
            session,
            site: site.to_string(),
            mirror,
            events,
        })
    }

    pub async fn is_available(&self) -> bool {
        self.mirror.lock().expect("mirror lock").session_alive
    }

    /// The TV's assigned user-profile uuid, empty string when unset —
    /// root wire shape.
    pub async fn get_user_profile(&self, tv: &str) -> Result<String> {
        let id = parse_tv(tv)?;
        Ok(self
            .mirror
            .lock()
            .expect("mirror lock")
            .assignments
            .get(&id)
            .map(Uuid::to_string)
            .unwrap_or_default())
    }

    pub async fn set_tv_user_profile(&self, tv: &str, profile: &str) -> Result<()> {
        let tv = parse_tv(tv)?;
        let profile: Option<Uuid> = if profile.is_empty() {
            None
        } else {
            Some(profile.parse().context("user profile uuid")?)
        };
        self.call(
            &keys::user_profile_rpc(&self.site, "assign"),
            serde_json::to_value(rpc::UserAssign { tv, profile })?,
        )
        .await
        .map(|_| ())
    }

    pub async fn list_user_profile_bodies(&self) -> Result<HashMap<Uuid, UserProfileBody>> {
        Ok(self
            .mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .iter()
            .map(|(id, body)| (*id, body.clone()))
            .collect())
    }

    pub async fn get_user_profile_body(&self, profile: Uuid) -> Result<UserProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
            .with_context(|| format!("no user profile {profile}"))
    }

    pub async fn create_user_profile(&self, body: &UserProfileBody) -> Result<Uuid> {
        let ok = self
            .call(
                &keys::user_profile_rpc(&self.site, "create"),
                json!({ "body": body }),
            )
            .await?;
        ok["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .context("create reply carries the id")
    }

    pub async fn update_user_profile(&self, profile: Uuid, body: &UserProfileBody) -> Result<()> {
        self.call(
            &keys::user_profile_rpc(&self.site, "update"),
            json!({ "id": profile, "body": body }),
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_user_profile(&self, profile: Uuid) -> Result<()> {
        self.call(
            &keys::user_profile_rpc(&self.site, "delete"),
            json!({ "id": profile }),
        )
        .await
        .map(|_| ())
    }

    pub async fn duplicate_user_profile(&self, profile: Uuid, name: &str) -> Result<Uuid> {
        let ok = self
            .call(
                &keys::user_profile_rpc(&self.site, "duplicate"),
                json!({ "id": profile, "name": name }),
            )
            .await?;
        ok["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .context("duplicate reply carries the id")
    }

    pub async fn get_resolved_user_profile_defaults(&self) -> Result<Value> {
        self.call(&keys::user_profile_rpc(&self.site, "defaults"), Value::Null)
            .await
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn call(&self, key: &str, body: Value) -> Result<Value> {
        call_rpc(&self.session, key, body).await
    }
}

/// The spike tray publishes no settings service — every method reports
/// unavailability so the settings pane renders its degraded state.
#[derive(Debug, Clone, Default)]
pub struct TrayClient;

impl TrayClient {
    pub async fn is_available(&self) -> bool {
        false
    }

    pub async fn notification_settings(&self) -> Result<NotificationSettingsBody> {
        bail!("tray settings service not available in the zenoh spike")
    }

    pub async fn update_notification_settings(
        &self,
        _body: &NotificationSettingsBody,
    ) -> Result<()> {
        bail!("tray settings service not available in the zenoh spike")
    }

    pub async fn send_test_notification(&self, _category: &str) -> Result<()> {
        bail!("tray settings service not available in the zenoh spike")
    }
}

/// Root-compatible watcher config; the zenoh watcher forwards mirror
/// deltas directly, so only the fallback interval is load-bearing.
#[derive(Debug, Clone)]
pub struct StateWatcherConfig {
    pub signal_debounce: Duration,
    pub fallback_poll_interval: Duration,
}

impl Default for StateWatcherConfig {
    fn default() -> Self {
        Self {
            signal_debounce: Duration::from_millis(100),
            fallback_poll_interval: Duration::from_secs(300),
        }
    }
}

/// Merged change stream: daemon mirror deltas + optional session mirror
/// deltas + a fallback poll tick.
#[derive(Debug)]
pub struct StateWatcher {
    rx: mpsc::UnboundedReceiver<StateEvent>,
}

impl StateWatcher {
    pub fn new(
        daemon: &DaemonClient,
        session: Option<&SessionClient>,
        config: StateWatcherConfig,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        forward(daemon.subscribe_events(), tx.clone());
        if let Some(session) = session {
            forward(session.subscribe_events(), tx.clone());
        }
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.fallback_poll_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await; // immediate first tick is not a poll
            loop {
                interval.tick().await;
                if tx.send(StateEvent::FallbackPoll).is_err() {
                    return;
                }
            }
        });
        Self { rx }
    }

    pub async fn next(&mut self) -> Option<StateEvent> {
        self.rx.recv().await
    }
}

fn forward(mut rx: broadcast::Receiver<StateEvent>, tx: mpsc::UnboundedSender<StateEvent>) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    warn!(missed, "state events lagged; forcing a refresh");
                    if tx.send(StateEvent::FallbackPoll).is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

fn parse_tv(tv: &str) -> Result<Uuid> {
    tv.parse().with_context(|| format!("invalid TV id '{tv}'"))
}

/// Redacted config JSON → display rows. The reply nests the vendor
/// table under `config`; top-level TV fields render elsewhere.
fn flatten_config(config: &Value) -> Vec<(String, String)> {
    let Some(vendor) = config.get("config").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows: Vec<(String, String)> = vendor
        .iter()
        .map(|(key, value)| {
            let rendered = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (key.clone(), rendered)
        })
        .collect();
    rows.sort();
    rows
}

async fn call_rpc(session: &Session, key: &str, body: Value) -> Result<Value> {
    let replies = session
        .get(key.to_string())
        .payload(serde_json::to_vec(&body)?)
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(RPC_TIMEOUT)
        .await
        .map_err(|e| anyhow!("rpc {key}: {e}"))?;
    let reply = replies
        .recv_async()
        .await
        .map_err(|_| anyhow!("rpc {key}: no reply (daemon down?)"))?;
    match reply.result() {
        Ok(sample) => {
            Ok(serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null))
        }
        Err(err) => {
            let error: tvnext_wire::error::WireError =
                serde_json::from_slice(&err.payload().to_bytes()).unwrap_or_else(|_| {
                    tvnext_wire::error::WireError {
                        code: "unknown".into(),
                        message: "undecodable error payload".into(),
                    }
                });
            bail!("{} ({})", error.message, error.code)
        }
    }
}

mod mirror_row {
    //! `TvRow` → `TvInfo` projection, kept next to the type it fills.

    use super::*;
    use tvnext_wire::payload;

    pub(crate) struct Row {
        power: Option<payload::Power>,
        volume: Option<payload::Volume>,
        link: Option<payload::Link>,
        input: Option<payload::Input>,
        backlight: Option<payload::Backlight>,
        identity: Option<payload::Identity>,
        meta: Option<payload::Meta>,
    }

    impl From<&crate::mirror::TvRow> for Row {
        fn from(row: &crate::mirror::TvRow) -> Self {
            Self {
                power: row.power.clone(),
                volume: row.volume.clone(),
                link: row.link.clone(),
                input: row.input.clone(),
                backlight: row.backlight.clone(),
                identity: row.identity.clone(),
                meta: row.meta.clone(),
            }
        }
    }

    impl Row {
        pub(crate) fn into_tv_info(
            self,
            id: Uuid,
            site: &str,
            vendor_config: Vec<(String, String)>,
        ) -> TvInfo {
            let identity = self
                .identity
                .unwrap_or_else(|| payload::Identity::new(String::new(), String::new()));
            let meta = self.meta.unwrap_or_else(|| payload::Meta::new(false));
            let caps: Vec<&str> = identity.capabilities.iter().map(String::as_str).collect();
            let power_state = self
                .power
                .as_ref()
                .map(|p| {
                    serde_json::to_value(p.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| "unknown".into());
            TvInfo {
                uuid: id.to_string(),
                name: identity.name.clone(),
                vendor: identity.vendor.clone(),
                brand: identity
                    .brand
                    .clone()
                    .unwrap_or_else(|| identity.vendor.clone()),
                capabilities: Capabilities::from_strings(&caps),
                connected: self.link.as_ref().is_some_and(|l| l.connected),
                managed: meta.managed,
                automated: meta.automated,
                system_profile: meta
                    .system_profile
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
                primary_input: meta.primary_input.clone().unwrap_or_default(),
                hardware_id: HardwareId {
                    manufacturer: identity.make.clone(),
                    model: identity.model.clone(),
                    serial: identity.serial.clone(),
                },
                vendor_config,
                vendor_info: Vec::new(),
                desktop: DesktopMapping {
                    connector: identity.connector.clone().unwrap_or_default(),
                },
                path: keys::tv_path(site, &id, ""),
                power_state,
                supports_wake: self.link.as_ref().is_some_and(|l| l.supports_wake),
                is_screen_on: self.power.as_ref().is_some_and(|p| p.is_screen_on),
                volume: self.volume.as_ref().map(|v| v.level).unwrap_or_default(),
                muted: self.volume.as_ref().is_some_and(|v| v.muted),
                current_input: self
                    .input
                    .as_ref()
                    .and_then(|i| i.current.clone())
                    .unwrap_or_default(),
                backlight: self.backlight.as_ref().map(|b| b.level).unwrap_or_default(),
                inputs: self
                    .input
                    .as_ref()
                    .map(|i| {
                        i.available
                            .iter()
                            .map(|entry| (entry.id.clone(), entry.label.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        }
    }
}
