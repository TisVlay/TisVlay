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
use tisvlay_wire::{bootstrap, keys, rpc};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;
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

/// The first key in a change window — root parity for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvStateChange {
    pub key: String,
}

/// Change notifications the GUI's refresh dispatcher consumes. Variant
/// set mirrors the root watcher, including the tray settings-service
/// events (liveliness + settings republish).
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

/// Root-compatible notification settings body — the partial-patch shape
/// of the tray's get/update settings rpcs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationSettingsBody {
    pub tv_connection: Option<bool>,
    pub tv_pairing: Option<bool>,
    pub tv_wake: Option<bool>,
    pub daemon_offline: Option<bool>,
}

impl NotificationSettingsBody {
    /// Factory defaults — every category at its default-on value.
    pub fn defaults() -> Self {
        let mut body = Self::default();
        for category in NotificationCategory::ALL {
            category.set(&mut body, category.default_enabled());
        }
        body
    }
}

/// Root-compatible notification category vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotificationCategory {
    TvConnection = 0,
    TvPairing = 1,
    TvWake = 2,
    DaemonOffline = 3,
}

impl NotificationCategory {
    pub const ALL: [Self; 4] = [
        Self::TvConnection,
        Self::TvPairing,
        Self::TvWake,
        Self::DaemonOffline,
    ];

    pub const fn bit(self) -> u8 {
        1u8 << (self as u8)
    }

    const fn key(self) -> &'static str {
        match self {
            Self::TvConnection => "tv_connection",
            Self::TvPairing => "tv_pairing",
            Self::TvWake => "tv_wake",
            Self::DaemonOffline => "daemon_offline",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|category| category.key() == key)
    }

    pub fn get(self, body: &NotificationSettingsBody) -> Option<bool> {
        match self {
            Self::TvConnection => body.tv_connection,
            Self::TvPairing => body.tv_pairing,
            Self::TvWake => body.tv_wake,
            Self::DaemonOffline => body.daemon_offline,
        }
    }

    pub fn set(self, body: &mut NotificationSettingsBody, value: bool) {
        match self {
            Self::TvConnection => body.tv_connection = Some(value),
            Self::TvPairing => body.tv_pairing = Some(value),
            Self::TvWake => body.tv_wake = Some(value),
            Self::DaemonOffline => body.daemon_offline = Some(value),
        }
    }

    pub const fn default_enabled(self) -> bool {
        match self {
            Self::TvConnection => true,
            Self::TvPairing => true,
            Self::TvWake => true,
            Self::DaemonOffline => true,
        }
    }
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
        let session = zenoh::open(bootstrap::client_config_resilient(socket)?)
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
            let vendor_config = match self.get_tv_config_value(&id.to_string()).await {
                Ok(config) => flatten_config(&config),
                Err(_) => Vec::new(),
            };
            tvs.push(row.into_tv_info(id, &self.site, vendor_config));
        }
        Ok(tvs)
    }

    pub async fn version(&self) -> Result<String> {
        let info = self
            .call(&keys::manager_rpc(&self.site, "info"), Value::Null)
            .await?;
        Ok(info["version"].as_str().unwrap_or_default().to_string())
    }

    /// The daemon's state-store path (the zenoh stack has no config
    /// file; the store is what an admin would inspect).
    pub async fn config_path(&self) -> Result<String> {
        let info = self
            .call(&keys::manager_rpc(&self.site, "info"), Value::Null)
            .await?;
        Ok(info["state_path"].as_str().unwrap_or_default().to_string())
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

    pub async fn pair_tv(
        &self,
        name: &str,
        vendor: &str,
        config_json: &str,
        managed: bool,
    ) -> Result<String> {
        let config: Value = serde_json::from_str(config_json).context("pairing config JSON")?;
        let body = serde_json::to_value(rpc::AddTv {
            id: None,
            name: name.into(),
            vendor: vendor.into(),
            managed,
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

    /// Bus-wide pairing progress (root shape): every session's samples
    /// as `(tv_id, phase, detail)` tuples, hydrated from the deep
    /// pairing caches so a re-opened wizard replays prior phases.
    pub async fn pairing_progress_stream(
        &self,
    ) -> Result<impl futures_util::Stream<Item = (String, String, PairingProgressDetail)> + use<>>
    {
        let sub = self
            .session
            .declare_subscriber(format!("tisvlay/{}/pairing/*/progress", self.site))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe pairing progress: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let site = self.site.clone();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Some(tv) = keys::parse_pairing_progress(&site, sample.key_expr().as_str())
                else {
                    continue;
                };
                let Ok(progress) = serde_json::from_slice::<Value>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                let item = (
                    tv.to_string(),
                    progress["phase"].as_str().unwrap_or_default().to_string(),
                    PairingProgressDetail {
                        message: progress["message"].as_str().unwrap_or_default().to_string(),
                        code: progress["code"].as_str().map(str::to_string),
                        path: progress["path"].as_str().map(str::to_string),
                    },
                );
                if tx.send(item).is_err() {
                    return;
                }
            }
        });
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Drain a pairing-progress stream to `tv_id`'s terminal phase,
    /// reporting non-terminal phases through `on_progress` — root
    /// semantics verbatim.
    pub async fn drain_pair_to_terminal<S>(
        stream: S,
        target_tv_id: &str,
        on_progress: impl FnMut(&str, &str),
    ) -> Result<PairTerminal>
    where
        S: futures_util::Stream<Item = (String, String, PairingProgressDetail)>,
    {
        let mut stream = std::pin::pin!(stream);
        Self::drain_pair_to_terminal_pinned(stream.as_mut(), target_tv_id, on_progress).await
    }

    /// Pinned-borrow workhorse for [`Self::drain_pair_to_terminal`] —
    /// for callers that keep the stream alive across operations (wait
    /// for `awaiting_tv`, cancel, then drain the remainder).
    pub async fn drain_pair_to_terminal_pinned<S>(
        mut stream: std::pin::Pin<&mut S>,
        target_tv_id: &str,
        mut on_progress: impl FnMut(&str, &str),
    ) -> Result<PairTerminal>
    where
        S: futures_util::Stream<Item = (String, String, PairingProgressDetail)>,
    {
        use futures_util::StreamExt;
        while let Some((tv_id, phase, detail)) = stream.as_mut().next().await {
            if tv_id != target_tv_id {
                continue;
            }
            match phase.as_str() {
                "complete" => {
                    return Ok(PairTerminal::Complete {
                        tv_id,
                        message: detail.message,
                    });
                }
                "failed" => return Ok(PairTerminal::Failed { tv_id, detail }),
                _ => on_progress(&phase, &detail.message),
            }
        }
        bail!(
            "PairingProgress stream ended before reaching a terminal phase for tv_id `{target_tv_id}`"
        )
    }

    /// One TV's `progress/power` phases (wake feedback), root item type.
    pub async fn power_cycle_progress_stream(
        &self,
        tv: &str,
    ) -> Result<impl futures_util::Stream<Item = tisvlay_core::PowerCyclePhase> + use<>> {
        use tisvlay_core::PowerCyclePhase as P;
        let id = parse_tv(tv)?;
        let sub = self
            .session
            .declare_subscriber(keys::tv_path(&self.site, &id, "progress/power"))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe power progress: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Ok(progress) = serde_json::from_slice::<Value>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                let phase = [
                    P::WolSent,
                    P::DirectConnect,
                    P::Connecting,
                    P::Connected,
                    P::Failed,
                ]
                .into_iter()
                .find(|phase| Some(phase.as_str()) == progress["phase"].as_str());
                if let Some(phase) = phase
                    && tx.send(phase).is_err()
                {
                    return;
                }
            }
        });
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Open-ended discovery session. The wire scan runs its own cadence;
    /// `timeout_ms` is accepted for root-signature parity but the
    /// session lives until [`Self::stop_discovery`].
    pub async fn start_discovery(&self, _timeout_ms: u32) -> Result<String> {
        let body = serde_json::to_value(rpc::DiscoverStart { vendor: None })?;
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

    /// Bus-wide findings (root item shape): every discovery session.s
    /// puts as `(id, name, address, hardware_id, vendor, vendor_data)`.
    #[allow(clippy::type_complexity)]
    pub async fn discovery_found_stream(
        &self,
    ) -> Result<
        impl futures_util::Stream<
            Item = (
                String,
                String,
                String,
                HardwareId,
                String,
                HashMap<String, String>,
            ),
        > + use<>,
    > {
        let sub = self
            .session
            .declare_subscriber(format!("tisvlay/{}/discovery/*/found/*", self.site))
            .await
            .map_err(|e| anyhow!("subscribe discovery findings: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Ok(found) =
                    serde_json::from_slice::<DiscoveredTv>(&sample.payload().to_bytes())
                else {
                    continue;
                };
                let item = (
                    found.id,
                    found.name,
                    found.address,
                    found.hardware_id.unwrap_or_default(),
                    found.vendor,
                    found.vendor_data,
                );
                if tx.send(item).is_err() {
                    return;
                }
            }
        });
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Manual-entry hint: the daemon probes its ARP cache (after the
    /// vendor primes it) so the wizard can show the device's MAC before
    /// pairing. Pair-time enrichment daemon-side remains the safety net.
    pub async fn enrich_manual_entry(
        &self,
        vendor: &str,
        address: &str,
    ) -> Result<HashMap<String, String>> {
        let map = self
            .call(
                &keys::manager_rpc(&self.site, "enrich"),
                json!({ "vendor": vendor, "ip": address }),
            )
            .await?;
        Ok(map
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn get_vendor_capabilities(&self, vendor: &str) -> Result<Capabilities> {
        let list = self.call(&keys::vendors(&self.site), Value::Null).await?;
        let names: Vec<String> = list
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["id"] == vendor)
            .map(|entry| {
                entry["capabilities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|cap| cap.as_str().map(str::to_string))
                    .collect()
            })
            .with_context(|| format!("no vendor '{vendor}'"))?;
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        Ok(Capabilities::from_strings(&refs))
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

    pub async fn set_mute(&self, tv: &str, mute: bool) -> Result<()> {
        self.tv_call(tv, "set-muted", json!({ "muted": mute }))
            .await
    }

    pub async fn set_backlight(&self, tv: &str, level: u8) -> Result<()> {
        self.tv_call(tv, "set-backlight", json!({ "level": level }))
            .await
    }

    pub async fn set_name(&self, tv: &str, name: &str) -> Result<()> {
        self.tv_call(tv, "set-name", json!({ "name": name })).await
    }

    pub async fn set_tv_managed(&self, tv: &str, managed: bool) -> Result<()> {
        self.tv_call(tv, "set-managed", json!({ "managed": managed }))
            .await
    }

    pub async fn set_tv_primary_input(&self, tv: &str, input: &str) -> Result<()> {
        let input = (!input.is_empty()).then_some(input);
        self.tv_call(tv, "set-primary-input", json!({ "input": input }))
            .await
    }

    pub async fn set_automate_when(&self, tv: &str, when: &str) -> Result<()> {
        self.tv_call(tv, "set-automate-when", json!({ "when": when }))
            .await
    }

    pub async fn set_tv_system_profile(&self, tv: &str, profile_uuid: &str) -> Result<()> {
        let profile: Option<Uuid> = if profile_uuid.is_empty() {
            None
        } else {
            Some(profile_uuid.parse().context("system profile uuid")?)
        };
        self.tv_call(tv, "set-system-profile", json!({ "profile": profile }))
            .await
    }

    pub async fn reconnect(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "reconnect", Value::Null).await
    }

    pub async fn remove_tv(&self, tv: &str) -> Result<()> {
        self.tv_call(tv, "remove", Value::Null).await
    }

    pub async fn get_tv_config(&self, tv: &str) -> Result<String> {
        let config = self.get_tv_config_value(tv).await?;
        serde_json::to_string(&config).context("encode config JSON")
    }

    async fn get_tv_config_value(&self, tv: &str) -> Result<Value> {
        let id = parse_tv(tv)?;
        self.call(&keys::tv_rpc(&self.site, &id, "get-config"), Value::Null)
            .await
    }

    pub async fn update_tv_config(&self, tv: &str, patch_json: &str) -> Result<()> {
        let id = parse_tv(tv)?;
        let patch: Value = serde_json::from_str(patch_json).context("config patch JSON")?;
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

    pub async fn create_system_profile(&self, name: &str) -> Result<Uuid> {
        let body = SystemProfileBody {
            name: name.into(),
            ..Default::default()
        };
        let body = &body;
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
    pub async fn get_resolved_system_profile_defaults(&self) -> Result<SystemProfileBody> {
        let mut defaults = self
            .call(&keys::profile_rpc(&self.site, "defaults"), Value::Null)
            .await?;
        defaults["name"] = Value::String("default".into());
        serde_json::from_value(defaults).context("decode resolved system defaults")
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
        let session = zenoh::open(bootstrap::client_config_resilient(socket)?)
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

    pub async fn version(&self) -> Result<String> {
        let info = self
            .call(&keys::user_profile_rpc(&self.site, "info"), Value::Null)
            .await?;
        Ok(info["version"].as_str().unwrap_or_default().to_string())
    }

    pub async fn config_path(&self) -> Result<String> {
        let info = self
            .call(&keys::user_profile_rpc(&self.site, "info"), Value::Null)
            .await?;
        Ok(info["state_path"].as_str().unwrap_or_default().to_string())
    }

    pub async fn reload_config(&self) -> Result<()> {
        // The session store is wire-mutated; there is no file to reload.
        Ok(())
    }

    /// The TV's assigned user-profile uuid, empty string when unset —
    /// root wire shape.
    pub async fn get_tv_user_profile(&self, tv: &str) -> Result<String> {
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

    pub async fn get_user_profile(&self, profile: Uuid) -> Result<UserProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
            .with_context(|| format!("no user profile {profile}"))
    }

    pub async fn create_user_profile(&self, name: &str) -> Result<Uuid> {
        let body = UserProfileBody {
            name: name.into(),
            ..Default::default()
        };
        let body = &body;
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

    pub async fn get_resolved_user_profile_defaults(&self) -> Result<UserProfileBody> {
        let mut defaults = self
            .call(&keys::user_profile_rpc(&self.site, "defaults"), Value::Null)
            .await?;
        defaults["name"] = Value::String("default".into());
        serde_json::from_value(defaults).context("decode resolved user defaults")
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn call(&self, key: &str, body: Value) -> Result<Value> {
        call_rpc(&self.session, key, body).await
    }
}

/// Client for the tray's per-user settings service (user socket):
/// availability from its liveliness token, settings from the cached
/// `user/tray/settings` pub, writes via `user/tray/rpc/*`.
#[derive(Debug, Clone)]
pub struct TrayClient {
    session: Session,
    site: String,
    alive: Arc<Mutex<bool>>,
    events: broadcast::Sender<StateEvent>,
}

impl TrayClient {
    pub async fn connect(socket: &Path, site: &str) -> Result<Self> {
        let session = zenoh::open(bootstrap::client_config_resilient(socket)?)
            .await
            .map_err(|e| anyhow!("connect to tray socket {}: {e}", socket.display()))?;
        let alive = Arc::new(Mutex::new(false));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let liveliness = session
            .liveliness()
            .declare_subscriber(keys::user_alive_tray(site))
            .history(true)
            .await
            .map_err(|e| anyhow!("subscribe tray liveliness: {e}"))?;
        {
            let alive = Arc::clone(&alive);
            let events = events.clone();
            tokio::spawn(async move {
                while let Ok(sample) = liveliness.recv_async().await {
                    let up = sample.kind() == SampleKind::Put;
                    *alive.lock().expect("tray alive lock") = up;
                    let _ = events.send(if up {
                        StateEvent::TrayAppeared
                    } else {
                        StateEvent::TrayDisappeared
                    });
                }
            });
        }
        let settings_sub = session
            .declare_subscriber(keys::user_tray_settings(site))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| anyhow!("subscribe tray settings: {e}"))?;
        {
            let events = events.clone();
            tokio::spawn(async move {
                while settings_sub.recv_async().await.is_ok() {
                    let _ = events.send(StateEvent::TrayNotificationSettingsChanged);
                }
            });
        }
        Ok(Self {
            session,
            site: site.to_string(),
            alive,
            events,
        })
    }

    pub async fn is_available(&self) -> bool {
        *self.alive.lock().expect("tray alive lock")
    }

    pub async fn version(&self) -> Result<String> {
        let info = self.call("info", Value::Null).await?;
        Ok(info["version"].as_str().unwrap_or_default().to_string())
    }

    pub async fn config_path(&self) -> Result<String> {
        let info = self.call("info", Value::Null).await?;
        Ok(info["state_path"].as_str().unwrap_or_default().to_string())
    }

    pub async fn reload_config(&self) -> Result<()> {
        Ok(())
    }

    pub async fn notification_settings(&self) -> Result<NotificationSettingsBody> {
        let settings = self.call("get", Value::Null).await?;
        Ok(NotificationSettingsBody {
            tv_connection: settings["tv_connection"].as_bool(),
            tv_pairing: settings["tv_pairing"].as_bool(),
            tv_wake: settings["tv_wake"].as_bool(),
            daemon_offline: settings["daemon_offline"].as_bool(),
        })
    }

    pub async fn update_notification_settings(&self, body: NotificationSettingsBody) -> Result<()> {
        self.call("update", json!(body_map(&body)))
            .await
            .map(|_| ())
    }

    pub async fn send_test_notification(&self) -> Result<()> {
        self.call("test", Value::Null).await.map(|_| ())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn call(&self, verb: &str, body: Value) -> Result<Value> {
        call_rpc(&self.session, &keys::user_tray_rpc(&self.site, verb), body).await
    }
}

/// Partial update map: only the `Some` switches ride the patch.
fn body_map(body: &NotificationSettingsBody) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    let mut put = |key: &str, value: Option<bool>| {
        if let Some(value) = value {
            map.insert(key.into(), Value::Bool(value));
        }
    };
    put("tv_connection", body.tv_connection);
    put("tv_pairing", body.tv_pairing);
    put("tv_wake", body.tv_wake);
    put("daemon_offline", body.daemon_offline);
    map
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
        tray: Option<&TrayClient>,
        config: StateWatcherConfig,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        forward(daemon.subscribe_events(), tx.clone());
        if let Some(session) = session {
            forward(session.subscribe_events(), tx.clone());
        }
        if let Some(tray) = tray {
            forward(tray.subscribe_events(), tx.clone());
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
            let error: tisvlay_wire::error::WireError =
                serde_json::from_slice(&err.payload().to_bytes()).unwrap_or_else(|_| {
                    tisvlay_wire::error::WireError {
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
    use tisvlay_wire::payload;

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
