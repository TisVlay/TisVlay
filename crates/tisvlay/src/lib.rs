//! Client SDK for the TisVlay services: [`DaemonClient`],
//! [`SessionClient`] and [`TrayClient`], plus a [`StateWatcher`] merging
//! their change events.
//!
//! Reads come from client-side mirrors: history subscribers over the
//! services' cached keys. Writes are typed rpcs, and change events are
//! the mirrors' own deltas. Failures are one [`Error`], named for what a
//! caller can act on.

mod desktop;
mod error;
mod mirror;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tisvlay_core::{
    Capabilities, DiscoveredTv, HardwareId, PairingField, SystemProfileConfig, UserProfileConfig,
};
use tisvlay_wire::caller::Caller;
use tisvlay_wire::endpoint::{
    Unary, manager, profile, tv, user_profile, user_remote, user_tray, vendors,
};
use tisvlay_wire::{bootstrap, keys, rpc};

pub use desktop::DesktopMapping;
pub use error::{BoxError, Error, Result, Service};
pub use tisvlay_wire::error::ErrorCode;
pub use tisvlay_wire::payload::{
    FailureCode, LinkMacOutcome, LinkMacReport, PairingPhase, PairingProgress, PowerPhase,
    RemoteSessionState, RemoteSource, RemoteSourceKind,
};
pub use tisvlay_wire::rpc::{
    NotificationSettings, NotificationSettingsPatch, PairStarted, ReloadReport, TvConfigView,
    VendorEntry,
};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use mirror::{SystemMirror, UserMirror};

/// Profile bodies: the wire carries the config structs verbatim (name
/// plus per-field options).
pub type SystemProfileBody = SystemProfileConfig;
pub type UserProfileBody = UserProfileConfig;

/// Terminal pairing outcome, drained from the progress key.
#[derive(Debug, Clone)]
pub enum PairTerminal {
    Complete {
        tv: Uuid,
        message: String,
        /// The device-reported link MAC and what the daemon did with it.
        link_mac: Option<LinkMacReport>,
    },
    Failed {
        tv: Uuid,
        /// The `failed` sample: its message, reason `code` and `path`.
        progress: PairingProgress,
    },
}

/// The first key in a change window, for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvStateChange {
    pub key: String,
}

/// Change notifications from the clients' mirrors and liveliness, plus
/// the tray settings service's events (liveliness and settings
/// republish).
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
    RemoteSessionChanged,
    TrayAppeared,
    TrayDisappeared,
    TrayNotificationSettingsChanged,
    FallbackPoll,
}

/// The tray's notification categories, keyed by their settings field
/// names.
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

    /// This category's switch in the full settings.
    pub fn get(self, settings: &NotificationSettings) -> bool {
        match self {
            Self::TvConnection => settings.tv_connection,
            Self::TvPairing => settings.tv_pairing,
            Self::TvWake => settings.tv_wake,
            Self::DaemonOffline => settings.daemon_offline,
        }
    }

    /// Set this category's switch in an update.
    pub fn set(self, patch: &mut NotificationSettingsPatch, value: bool) {
        match self {
            Self::TvConnection => patch.tv_connection = Some(value),
            Self::TvPairing => patch.tv_pairing = Some(value),
            Self::TvWake => patch.tv_wake = Some(value),
            Self::DaemonOffline => patch.daemon_offline = Some(value),
        }
    }
}

/// Everything `list_tvs` reports per TV, projected from the mirror.
#[derive(Debug, Clone, Default)]
pub struct TvInfo {
    pub uuid: Uuid,
    pub name: String,
    pub vendor: String,
    pub brand: String,
    pub capabilities: Capabilities,
    pub connected: bool,
    pub managed: bool,
    pub automated: bool,
    /// Whether a remote session currently arms this TV — the read-only
    /// projection of the world's `remote_armed` gate.
    pub remote_armed: bool,
    /// The assigned system profile; `None` uses the default.
    pub system_profile: Option<Uuid>,
    pub primary_input: String,
    pub hardware_id: HardwareId,
    /// The vendor's primary identifying descriptor (LG: IP; CEC: device
    /// node) — the connector's `primary_descriptor`, published on the
    /// TV's identity.
    pub primary_descriptor: String,
    /// The connector's `display_config` projection: schema-ordered,
    /// labeled `(label, value)` rows of the user-editable config,
    /// published as `meta/vendor` state.
    pub vendor_config: Vec<(String, String)>,
    /// The connector's `display_diagnostics` projection (pair-time
    /// captures, e.g. LG's WebSocket scheme), from the same payload.
    pub vendor_info: Vec<(String, String)>,
    pub desktop: DesktopMapping,
    pub path: String,
    pub power_state: tisvlay_core::PowerState,
    pub supports_wake: bool,
    pub is_screen_on: bool,
    pub volume: u8,
    pub muted: bool,
    pub current_input: String,
    pub backlight: u8,
    pub inputs: Vec<(String, String)>,
}

const EVENT_CAPACITY: usize = 256;

/// Client for the system daemon: mirror reads, rpc writes.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    session: Session,
    caller: Arc<Caller>,
    site: String,
    mirror: Arc<Mutex<SystemMirror>>,
    events: broadcast::Sender<StateEvent>,
}

impl DaemonClient {
    /// Connect to the daemon socket and start mirroring.
    pub async fn connect(socket: &Path, site: &str) -> Result<Self> {
        let session = zenoh::open(
            bootstrap::client_config_resilient(socket)
                .map_err(|e| Error::transport("build the client config", e))?,
        )
        .await
        .map_err(|e| Error::transport("connect to the daemon socket", e))?;
        let mirror = Arc::new(Mutex::new(SystemMirror::default()));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);

        for key in [keys::tv_all(site), keys::profile_system_wild(site)] {
            let sub = session
                .declare_subscriber(key.as_str())
                .history(HistoryConfig::default().detect_late_publishers())
                .await
                .map_err(|e| Error::transport("subscribe to the daemon's keys", e))?;
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
            .map_err(|e| Error::transport("subscribe to the daemon's liveliness", e))?;
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
            caller: Arc::new(Caller::new(session.clone(), site)),
            session,
            site: site.to_string(),
            mirror,
            events,
        })
    }

    pub async fn is_available(&self) -> bool {
        self.mirror.lock().expect("mirror lock").daemon_alive
    }

    /// Assemble the fleet snapshot the GUI renders from — a pure mirror
    /// projection, no rpcs: the vendor display rows and the primary
    /// descriptor arrive as published state (`meta/vendor`,
    /// `meta/identity`) like everything else.
    pub async fn list_tvs(&self) -> Result<Vec<TvInfo>> {
        let rows: Vec<(Uuid, mirror_row::Row)> = {
            let mirror = self.mirror.lock().expect("mirror lock");
            mirror
                .tvs
                .iter()
                .map(|(id, row)| (*id, mirror_row::Row::from(row)))
                .collect()
        };
        Ok(rows
            .into_iter()
            .map(|(id, row)| row.into_tv_info(id, &self.site))
            .collect())
    }

    pub async fn version(&self) -> Result<String> {
        Ok(self.call::<manager::Info>(&(), &()).await?.version)
    }

    /// The daemon's state-store path (the zenoh stack has no config
    /// file; the store is what an admin would inspect).
    pub async fn config_path(&self) -> Result<String> {
        Ok(self.call::<manager::Info>(&(), &()).await?.state_path)
    }

    /// Apply a hand edit of the daemon's store file: only what changed
    /// since the daemon last wrote it applies, and the report says whether
    /// anything did. Refused when the file is missing or unparsable, or
    /// the daemon runs without one.
    pub async fn reload_config(&self) -> Result<ReloadReport> {
        self.call::<manager::Reload>(&(), &()).await
    }

    /// The daemon's vendor catalog.
    pub async fn get_supported_vendors(&self) -> Result<Vec<VendorEntry>> {
        self.call::<vendors::Catalog>(&(), &()).await
    }

    pub async fn get_pairing_schema(&self, vendor: &str) -> Result<Vec<PairingField>> {
        self.call::<vendors::Schema>(vendor, &()).await
    }

    pub async fn pair_tv(
        &self,
        name: &str,
        vendor: &str,
        config: serde_json::Map<String, Value>,
        managed: bool,
    ) -> Result<PairStarted> {
        let request = rpc::AddTv {
            id: None,
            name: name.into(),
            vendor: vendor.into(),
            managed,
            config,
        };
        self.call::<manager::Pair>(&(), &request).await
    }

    /// Start a re-pair; returns the attempt id its progress samples carry.
    pub async fn re_pair(&self, tv: Uuid) -> Result<Uuid> {
        let started = self.call::<tv::RePair>(&tv, &()).await?;
        Ok(started.attempt)
    }

    pub async fn cancel_pair(&self, tv: Uuid) -> Result<()> {
        self.call::<manager::CancelPair>(&(), &rpc::TvRef { id: tv })
            .await
            .map(|_| ())
    }

    /// Every pairing session's progress samples as `(tv, sample)`,
    /// hydrated from the deep pairing caches so a re-opened wizard
    /// replays prior phases.
    pub async fn pairing_progress_stream(
        &self,
    ) -> Result<impl futures_util::Stream<Item = (Uuid, PairingProgress)> + use<>> {
        let sub = self
            .session
            .declare_subscriber(keys::pairing_progress_wild(&self.site))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| Error::transport("subscribe to pairing progress", e))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let site = self.site.clone();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Some(tv) = keys::parse_pairing_progress(&site, sample.key_expr().as_str())
                else {
                    continue;
                };
                let Ok(progress) =
                    serde_json::from_slice::<PairingProgress>(&sample.payload().to_bytes())
                else {
                    warn!(%tv, "undecodable pairing progress sample skipped");
                    continue;
                };
                if tx.send((tv, progress)).is_err() {
                    return;
                }
            }
        });
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Drain a pairing-progress stream to `attempt`'s terminal phase on
    /// `tv`, reporting its non-terminal phases through `on_progress`.
    /// Other attempts' samples — earlier sessions replayed from the deep
    /// cache — are skipped, so a stale terminal can't end the drain.
    pub async fn drain_pair_to_terminal<S>(
        stream: S,
        tv: Uuid,
        attempt: Uuid,
        on_progress: impl FnMut(PairingPhase, &str),
    ) -> Result<PairTerminal>
    where
        S: futures_util::Stream<Item = (Uuid, PairingProgress)>,
    {
        let mut stream = std::pin::pin!(stream);
        Self::drain_pair_to_terminal_pinned(stream.as_mut(), tv, attempt, on_progress).await
    }

    /// Pinned-borrow workhorse for [`Self::drain_pair_to_terminal`] —
    /// for callers that keep the stream alive across operations (wait
    /// for `awaiting_tv`, cancel, then drain the remainder).
    pub async fn drain_pair_to_terminal_pinned<S>(
        mut stream: std::pin::Pin<&mut S>,
        tv: Uuid,
        attempt: Uuid,
        mut on_progress: impl FnMut(PairingPhase, &str),
    ) -> Result<PairTerminal>
    where
        S: futures_util::Stream<Item = (Uuid, PairingProgress)>,
    {
        use futures_util::StreamExt;
        while let Some((sample_tv, progress)) = stream.as_mut().next().await {
            if sample_tv != tv || progress.attempt != attempt {
                continue;
            }
            match progress.phase {
                PairingPhase::Complete => {
                    return Ok(PairTerminal::Complete {
                        tv,
                        message: progress.message,
                        link_mac: progress.link_mac,
                    });
                }
                PairingPhase::Failed => return Ok(PairTerminal::Failed { tv, progress }),
                phase => on_progress(phase, &progress.message),
            }
        }
        Err(Error::Transport {
            what: "the pairing progress stream ended before a terminal phase",
            source: None,
        })
    }

    /// One TV's `progress/power` phases (wake feedback).
    pub async fn power_cycle_progress_stream(
        &self,
        tv: Uuid,
    ) -> Result<impl futures_util::Stream<Item = PowerPhase> + use<>> {
        let sub = self
            .session
            .declare_subscriber(keys::tv_path(&self.site, &tv, "progress/power"))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| Error::transport("subscribe to power progress", e))?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Ok(progress) = serde_json::from_slice::<tisvlay_wire::payload::PowerProgress>(
                    &sample.payload().to_bytes(),
                ) else {
                    continue;
                };
                if tx.send(progress.phase).is_err() {
                    return;
                }
            }
        });
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Open an open-ended discovery session; it scans on the daemon's
    /// cadence until [`Self::stop_discovery`].
    pub async fn start_discovery(&self) -> Result<Uuid> {
        let started = self
            .call::<manager::DiscoverStart>(&(), &rpc::DiscoverStart { vendor: None })
            .await?;
        Ok(started.session)
    }

    pub async fn stop_discovery(&self, session: Uuid) -> Result<()> {
        self.call::<manager::DiscoverStop>(&(), &rpc::DiscoverStop { session })
            .await
            .map(|_| ())
    }

    /// Every discovery session's findings.
    pub async fn discovery_found_stream(
        &self,
    ) -> Result<impl futures_util::Stream<Item = DiscoveredTv> + use<>> {
        let sub = self
            .session
            .declare_subscriber(keys::discovery_found_wild(&self.site))
            .await
            .map_err(|e| Error::transport("subscribe to discovery findings", e))?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                let Ok(found) =
                    serde_json::from_slice::<DiscoveredTv>(&sample.payload().to_bytes())
                else {
                    warn!("undecodable discovery finding skipped");
                    continue;
                };
                if tx.send(found).is_err() {
                    return;
                }
            }
        });
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Manual-entry hint: the daemon probes its ARP cache (after the
    /// vendor primes it) for the device's MAC, so the wizard can show it
    /// before pairing. Pair-time enrichment daemon-side remains the
    /// safety net.
    pub async fn enrich_manual_entry(&self, vendor: &str, address: &str) -> Result<Option<String>> {
        let request = rpc::Enrich {
            vendor: vendor.into(),
            ip: address.into(),
        };
        Ok(self.call::<manager::Enrich>(&(), &request).await?.mac)
    }

    pub async fn power_on(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::PowerOn>(tv, &rpc::PowerOn::default())
            .await
    }

    pub async fn power_off(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::PowerOff>(tv, &()).await
    }

    pub async fn screen_on(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::SetScreen>(tv, &rpc::SetScreen::manual(true))
            .await
    }

    pub async fn screen_off(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::SetScreen>(tv, &rpc::SetScreen::manual(false))
            .await
    }

    pub async fn switch_input(&self, tv: Uuid, input: &str) -> Result<()> {
        self.tv_call::<tv::SwitchInput>(
            tv,
            &rpc::SwitchInput {
                input: input.into(),
            },
        )
        .await
    }

    pub async fn set_volume(&self, tv: Uuid, level: u8) -> Result<()> {
        self.tv_call::<tv::SetVolume>(tv, &rpc::SetVolume { level })
            .await
    }

    pub async fn set_muted(&self, tv: Uuid, muted: bool) -> Result<()> {
        self.tv_call::<tv::SetMuted>(tv, &rpc::SetMuted { muted })
            .await
    }

    pub async fn set_backlight(&self, tv: Uuid, level: u8) -> Result<()> {
        self.tv_call::<tv::SetBacklight>(tv, &rpc::SetBacklight { level })
            .await
    }

    pub async fn set_name(&self, tv: Uuid, name: &str) -> Result<()> {
        self.tv_call::<tv::SetName>(tv, &rpc::SetName { name: name.into() })
            .await
    }

    pub async fn set_tv_managed(&self, tv: Uuid, managed: bool) -> Result<()> {
        self.tv_call::<tv::SetManaged>(tv, &rpc::SetManaged { managed })
            .await
    }

    pub async fn set_tv_primary_input(&self, tv: Uuid, input: Option<&str>) -> Result<()> {
        let input = input.map(str::to_string);
        self.tv_call::<tv::SetPrimaryInput>(tv, &rpc::SetPrimaryInput { input })
            .await
    }

    pub async fn set_tv_system_profile(&self, tv: Uuid, profile: Option<Uuid>) -> Result<()> {
        self.tv_call::<tv::SetSystemProfile>(tv, &rpc::SetSystemProfile { profile })
            .await
    }

    pub async fn reconnect(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::Reconnect>(tv, &()).await
    }

    pub async fn remove_tv(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::Remove>(tv, &()).await
    }

    /// The TV's config, schema-secret fields removed.
    pub async fn get_tv_config(&self, tv: Uuid) -> Result<TvConfigView> {
        self.call::<tv::GetConfig>(&tv, &()).await
    }

    /// Patch the TV's config: known top-level keys apply to its settings,
    /// everything else merges into the vendor table.
    pub async fn update_tv_config(
        &self,
        tv: Uuid,
        patch: serde_json::Map<String, Value>,
    ) -> Result<()> {
        self.call::<tv::UpdateConfig>(&tv, &rpc::UpdateConfig(patch))
            .await
            .map(|_| ())
    }

    /// A system profile's body, from the mirror.
    pub async fn get_system_profile(&self, profile: Uuid) -> Option<SystemProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
    }

    pub async fn list_system_profile_bodies(&self) -> HashMap<Uuid, SystemProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .iter()
            .map(|(id, body)| (*id, body.clone()))
            .collect()
    }

    pub async fn create_system_profile(&self, name: &str) -> Result<Uuid> {
        let body = SystemProfileBody {
            name: name.into(),
            ..Default::default()
        };
        let request = rpc::ProfileCreate { id: None, body };
        Ok(self.call::<profile::Create>(&(), &request).await?.id)
    }

    pub async fn update_system_profile(
        &self,
        profile: Uuid,
        body: SystemProfileBody,
    ) -> Result<()> {
        let request = rpc::ProfileUpdate { id: profile, body };
        self.call::<profile::Update>(&(), &request)
            .await
            .map(|_| ())
    }

    pub async fn delete_system_profile(&self, profile: Uuid) -> Result<()> {
        self.call::<profile::Delete>(&(), &rpc::TvRef { id: profile })
            .await
            .map(|_| ())
    }

    pub async fn duplicate_system_profile(&self, profile: Uuid, name: &str) -> Result<Uuid> {
        let request = rpc::ProfileDuplicate {
            id: profile,
            name: name.into(),
        };
        Ok(self.call::<profile::Duplicate>(&(), &request).await?.id)
    }

    /// The stock mapping as system-profile field values (JSON map).
    pub async fn get_resolved_system_profile_defaults(&self) -> Result<SystemProfileBody> {
        let mut defaults = self.call::<profile::Defaults>(&(), &()).await?;
        defaults.name = "default".into();
        Ok(defaults)
    }

    /// One TV's resolved mapping (assigned profile over stock).
    pub async fn get_effective_system_profile(&self, tv: Uuid) -> Result<SystemProfileConfig> {
        self.call::<profile::Effective>(&(), &rpc::TvRef { id: tv })
            .await
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    /// A per-TV rpc that only acknowledges.
    async fn tv_call<R: Unary<Target = Uuid, Response = rpc::Ack>>(
        &self,
        tv: Uuid,
        request: &R::Request,
    ) -> Result<()> {
        self.call::<R>(&tv, request).await.map(|_| ())
    }

    async fn call<R: Unary>(
        &self,
        target: &R::Target,
        request: &R::Request,
    ) -> Result<R::Response> {
        self.caller
            .call::<R>(target, request)
            .await
            .map_err(|e| Error::from_rpc(Service::Daemon, e))
    }
}

/// Client for the per-user session service (user profiles).
#[derive(Debug, Clone)]
pub struct SessionClient {
    caller: Arc<Caller>,
    mirror: Arc<Mutex<UserMirror>>,
    events: broadcast::Sender<StateEvent>,
}

impl SessionClient {
    pub async fn connect(socket: &Path, site: &str) -> Result<Self> {
        let session = zenoh::open(
            bootstrap::client_config_resilient(socket)
                .map_err(|e| Error::transport("build the client config", e))?,
        )
        .await
        .map_err(|e| Error::transport("connect to the session socket", e))?;
        let mirror = Arc::new(Mutex::new(UserMirror::default()));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let sub = session
            .declare_subscriber(keys::user_all(site))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| Error::transport("subscribe to the user keys", e))?;
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
            .map_err(|e| Error::transport("subscribe to the session's liveliness", e))?;
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
            caller: Arc::new(Caller::new(session, site)),
            mirror,
            events,
        })
    }

    pub async fn is_available(&self) -> bool {
        self.mirror.lock().expect("mirror lock").session_alive
    }

    pub async fn version(&self) -> Result<String> {
        Ok(self.call::<user_profile::Info>(&(), &()).await?.version)
    }

    pub async fn config_path(&self) -> Result<String> {
        Ok(self.call::<user_profile::Info>(&(), &()).await?.state_path)
    }

    /// Apply a hand edit of the session's store file (user profiles and
    /// assignments): only what changed since the session last wrote it
    /// applies. Refused when the file is missing or unparsable, or the
    /// session runs without one.
    pub async fn reload_config(&self) -> Result<ReloadReport> {
        self.call::<user_profile::Reload>(&(), &()).await
    }

    /// The TV's assigned user-profile uuid, from the mirror; `None` when
    /// it uses the default.
    pub async fn get_tv_user_profile(&self, tv: Uuid) -> Option<Uuid> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .assignments
            .get(&tv)
            .copied()
    }

    pub async fn set_tv_user_profile(&self, tv: Uuid, profile: Option<Uuid>) -> Result<()> {
        self.call::<user_profile::Assign>(&(), &rpc::UserAssign { tv, profile })
            .await
            .map(|_| ())
    }

    pub async fn list_user_profile_bodies(&self) -> HashMap<Uuid, UserProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .iter()
            .map(|(id, body)| (*id, body.clone()))
            .collect()
    }

    /// A user profile's body, from the mirror.
    pub async fn get_user_profile(&self, profile: Uuid) -> Option<UserProfileBody> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
    }

    pub async fn create_user_profile(&self, name: &str) -> Result<Uuid> {
        let body = UserProfileBody {
            name: name.into(),
            ..Default::default()
        };
        let request = rpc::UserProfileCreate { id: None, body };
        Ok(self.call::<user_profile::Create>(&(), &request).await?.id)
    }

    pub async fn update_user_profile(&self, profile: Uuid, body: UserProfileBody) -> Result<()> {
        let request = rpc::UserProfileUpdate { id: profile, body };
        self.call::<user_profile::Update>(&(), &request)
            .await
            .map(|_| ())
    }

    pub async fn delete_user_profile(&self, profile: Uuid) -> Result<()> {
        self.call::<user_profile::Delete>(&(), &rpc::TvRef { id: profile })
            .await
            .map(|_| ())
    }

    pub async fn duplicate_user_profile(&self, profile: Uuid, name: &str) -> Result<Uuid> {
        let request = rpc::ProfileDuplicate {
            id: profile,
            name: name.into(),
        };
        Ok(self
            .call::<user_profile::Duplicate>(&(), &request)
            .await?
            .id)
    }

    pub async fn get_resolved_user_profile_defaults(&self) -> Result<UserProfileBody> {
        let mut defaults = self.call::<user_profile::Defaults>(&(), &()).await?;
        defaults.name = "default".into();
        Ok(defaults)
    }

    /// Arm or disarm a manual remote-session source through the session
    /// service's verb rpc — the same escape hatch the CLI and the
    /// Sunshine `global_prep_cmd` hook drive.
    pub async fn remote_session_set(&self, source: &str, active: bool) -> Result<()> {
        let request = rpc::RemoteSet {
            source: Some(source.into()),
            active,
        };
        self.call::<user_remote::Set>(&(), &request)
            .await
            .map(|_| ())
    }

    /// The current remote-session fact: live sources plus the armed
    /// projection. Read from the session service directly (the cached
    /// mirror slot is the passive view).
    pub async fn remote_session_status(&self) -> Result<RemoteSessionState> {
        self.call::<user_remote::Status>(&(), &()).await
    }

    /// The remote-session fact as last mirrored from the cached fact key
    /// — the passive counterpart to [`Self::remote_session_status`],
    /// updated on every `RemoteSessionChanged` event.
    pub async fn remote_session(&self) -> Option<RemoteSessionState> {
        self.mirror
            .lock()
            .expect("mirror lock")
            .remote_session
            .clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn call<R: Unary>(
        &self,
        target: &R::Target,
        request: &R::Request,
    ) -> Result<R::Response> {
        self.caller
            .call::<R>(target, request)
            .await
            .map_err(|e| Error::from_rpc(Service::Session, e))
    }
}

/// Client for the tray's per-user settings service (user socket):
/// availability from its liveliness token, settings from the cached
/// `user/tray/settings` pub, writes via `user/tray/rpc/*`.
#[derive(Debug, Clone)]
pub struct TrayClient {
    caller: Arc<Caller>,
    alive: Arc<Mutex<bool>>,
    events: broadcast::Sender<StateEvent>,
}

impl TrayClient {
    pub async fn connect(socket: &Path, site: &str) -> Result<Self> {
        let session = zenoh::open(
            bootstrap::client_config_resilient(socket)
                .map_err(|e| Error::transport("build the client config", e))?,
        )
        .await
        .map_err(|e| Error::transport("connect to the tray socket", e))?;
        let alive = Arc::new(Mutex::new(false));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let liveliness = session
            .liveliness()
            .declare_subscriber(keys::user_alive_tray(site))
            .history(true)
            .await
            .map_err(|e| Error::transport("subscribe to the tray's liveliness", e))?;
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
            .map_err(|e| Error::transport("subscribe to the tray settings", e))?;
        {
            let events = events.clone();
            tokio::spawn(async move {
                while settings_sub.recv_async().await.is_ok() {
                    let _ = events.send(StateEvent::TrayNotificationSettingsChanged);
                }
            });
        }
        Ok(Self {
            caller: Arc::new(Caller::new(session, site)),
            alive,
            events,
        })
    }

    pub async fn is_available(&self) -> bool {
        *self.alive.lock().expect("tray alive lock")
    }

    pub async fn version(&self) -> Result<String> {
        Ok(self.call::<user_tray::Info>(&()).await?.version)
    }

    pub async fn config_path(&self) -> Result<String> {
        Ok(self.call::<user_tray::Info>(&()).await?.state_path)
    }

    /// Apply a hand edit of the tray's settings file. Refused when the
    /// file is missing or unparsable.
    pub async fn reload_config(&self) -> Result<ReloadReport> {
        self.call::<user_tray::Reload>(&()).await
    }

    pub async fn notification_settings(&self) -> Result<NotificationSettings> {
        self.call::<user_tray::Get>(&()).await
    }

    pub async fn update_notification_settings(
        &self,
        patch: NotificationSettingsPatch,
    ) -> Result<()> {
        self.call::<user_tray::Update>(&patch).await.map(|_| ())
    }

    pub async fn send_test_notification(&self) -> Result<()> {
        self.call::<user_tray::Test>(&()).await.map(|_| ())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn call<R: Unary<Target = ()>>(&self, request: &R::Request) -> Result<R::Response> {
        self.caller
            .call::<R>(&(), request)
            .await
            .map_err(|e| Error::from_rpc(Service::Tray, e))
    }
}

/// Watcher tuning: mirror deltas forward directly, so the one knob is
/// the fallback poll interval.
#[derive(Debug, Clone)]
pub struct StateWatcherConfig {
    pub fallback_poll_interval: Duration,
}

impl Default for StateWatcherConfig {
    fn default() -> Self {
        Self {
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
        // Synthesize `*Appeared` for peers already alive: their
        // liveliness edge fired into the client's broadcast before this
        // watcher subscribed (a tokio broadcast drops events sent with
        // no receivers), so consumers relying on edges alone would
        // never learn about a peer that came up between client
        // construction and watcher construction. A level-triggered
        // emit after subscription closes the window; consumers treat
        // repeated `*Appeared` idempotently.
        {
            let tx = tx.clone();
            let daemon = daemon.clone();
            let session = session.cloned();
            let tray = tray.cloned();
            tokio::spawn(async move {
                if daemon.is_available().await {
                    let _ = tx.send(StateEvent::DaemonAppeared);
                }
                if let Some(session) = session
                    && session.is_available().await
                {
                    let _ = tx.send(StateEvent::SessionAppeared);
                }
                if let Some(tray) = tray
                    && tray.is_available().await
                {
                    let _ = tx.send(StateEvent::TrayAppeared);
                }
            });
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
        vendor: Option<payload::VendorRows>,
    }

    impl From<&mirror::TvRow> for Row {
        fn from(row: &mirror::TvRow) -> Self {
            Self {
                power: row.power.clone(),
                volume: row.volume.clone(),
                link: row.link.clone(),
                input: row.input.clone(),
                backlight: row.backlight.clone(),
                identity: row.identity.clone(),
                meta: row.meta.clone(),
                vendor: row.vendor.clone(),
            }
        }
    }

    impl Row {
        pub(crate) fn into_tv_info(self, id: Uuid, site: &str) -> TvInfo {
            let identity = self
                .identity
                .unwrap_or_else(|| payload::Identity::new(String::new(), String::new()));
            let meta = self.meta.unwrap_or_else(|| payload::Meta::new(false));
            let caps: Vec<&str> = identity.capabilities.iter().map(String::as_str).collect();
            let power_state = self.power.as_ref().map(|p| p.state).unwrap_or_default();
            TvInfo {
                uuid: id,
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
                remote_armed: meta.remote_armed,
                system_profile: meta.system_profile,
                primary_input: meta.primary_input.unwrap_or_default(),
                hardware_id: HardwareId {
                    manufacturer: identity.make.clone(),
                    model: identity.model.clone(),
                    serial: identity.serial.clone(),
                },
                primary_descriptor: identity.primary.clone().unwrap_or_default(),
                vendor_config: self
                    .vendor
                    .as_ref()
                    .map(|v| v.config.clone())
                    .unwrap_or_default(),
                vendor_info: self
                    .vendor
                    .as_ref()
                    .map(|v| v.diagnostics.clone())
                    .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn sample(tv: Uuid, phase: PairingPhase, attempt: Uuid) -> (Uuid, PairingProgress) {
        (
            tv,
            PairingProgress {
                attempt,
                phase,
                message: format!("{phase} ({attempt})"),
                code: None,
                path: None,
                link_mac: None,
            },
        )
    }

    /// A re-pair's history subscriber replays the previous session's
    /// terminal; the drain must not mistake it for the current attempt's.
    #[tokio::test]
    async fn drain_skips_terminals_from_other_attempts() {
        let (tv, other, old, new) = (uuid(1), uuid(2), uuid(10), uuid(11));
        let stream = futures_util::stream::iter([
            sample(tv, PairingPhase::Complete, old),
            sample(other, PairingPhase::Complete, new),
            sample(tv, PairingPhase::AwaitingTv, new),
            sample(tv, PairingPhase::Failed, old),
            sample(tv, PairingPhase::Complete, new),
        ]);
        let mut phases = Vec::new();
        let terminal = DaemonClient::drain_pair_to_terminal(stream, tv, new, |phase, _| {
            phases.push(phase);
        })
        .await
        .unwrap();
        let expected = format!("complete ({new})");
        assert!(
            matches!(&terminal, PairTerminal::Complete { tv: t, message, .. } if *t == tv && *message == expected),
            "{terminal:?}"
        );
        assert_eq!(phases, [PairingPhase::AwaitingTv]);
    }

    /// The terminal's link-MAC report reaches the caller.
    #[tokio::test]
    async fn drain_carries_the_link_mac_report() {
        let report = LinkMacReport {
            reported: "84:90:0A:63:A9:94".into(),
            outcome: LinkMacOutcome::Differs {
                configured: "64:E4:A5:6C:AB:B4".into(),
            },
        };
        let (tv, mut progress) = sample(uuid(1), PairingPhase::Complete, uuid(10));
        progress.link_mac = Some(report.clone());
        let stream = futures_util::stream::iter([(tv, progress)]);
        let terminal = DaemonClient::drain_pair_to_terminal(stream, tv, uuid(10), |_, _| {})
            .await
            .unwrap();
        assert!(
            matches!(&terminal, PairTerminal::Complete { link_mac: Some(r), .. } if *r == report),
            "{terminal:?}"
        );
    }
}
