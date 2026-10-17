#![doc = include_str!("../README.md")]
// Published crate: every public item is documented (docs/build.md,
// first-publish checklist).
#![warn(missing_docs)]

mod desktop;
mod error;
mod handshake;
mod mirror;

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tisvlay_core::{
    Capabilities, DiscoveredTv, HardwareId, PairingField, SystemProfileConfig, UserProfileConfig,
};
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::{
    Unary, manager, profile, tv, user_profile, user_remote, user_tray, vendors,
};
use tisvlay_protocol::{bootstrap, keys, rpc};

pub use desktop::DesktopMapping;
pub use error::{BoxError, Error, Result, Service};
/// The default socket paths a client connects to: the daemon's
/// (`DaemonClient::connect`) and the per-user session service's, which
/// the tray's settings service shares (`SessionClient::connect`,
/// `TrayClient::connect`).
pub use tisvlay_protocol::bootstrap::{default_connect_socket_path, default_session_socket_path};
pub use tisvlay_protocol::error::ErrorCode;
pub use tisvlay_protocol::payload::{
    FailureCode, LinkMacOutcome, LinkMacReport, PairingPhase, PairingProgress, PowerPhase,
    RemoteSessionState, RemoteSource, RemoteSourceKind,
};
pub use tisvlay_protocol::rpc::{
    NotificationSettings, NotificationSettingsPatch, PairStarted, ReloadReport, TvConfigView,
    VendorEntry,
};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;
use uuid::Uuid;
use zenoh::Session;
use zenoh::sample::SampleKind;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

use handshake::Handshake;
use mirror::{SystemMirror, UserMirror};

/// Profile bodies: the wire carries the config structs verbatim (name
/// plus per-field options).
pub type SystemProfileBody = SystemProfileConfig;
/// A user profile's body, carried verbatim like
/// [`SystemProfileBody`].
pub type UserProfileBody = UserProfileConfig;

/// Terminal pairing outcome, drained from the progress key.
#[derive(Debug, Clone)]
pub enum PairTerminal {
    /// The TV paired and the daemon's first connection to it succeeded;
    /// it's enrolled and appears in [`DaemonClient::list_tvs`].
    Complete {
        /// The paired TV.
        tv: Uuid,
        /// The daemon's human-readable summary, for display.
        message: String,
        /// The device-reported link MAC and what the daemon did with it.
        link_mac: Option<LinkMacReport>,
    },
    /// The attempt ended without pairing; `progress.code` says why.
    Failed {
        /// The TV the attempt was for.
        tv: Uuid,
        /// The `failed` sample: its message, reason `code` and `path`.
        progress: PairingProgress,
    },
}

/// A change to one of a TV's published keys: each sample the mirror
/// applies is its own event.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TvStateChange {
    /// The full key that changed, under the TV's [`TvInfo::path`].
    pub key: String,
}

/// Change notifications from the clients' mirrors and liveliness, plus
/// the tray settings service's events (liveliness and settings
/// republish).
#[derive(Debug, Clone)]
pub enum StateEvent {
    /// A TV's published state changed in the daemon mirror: a TV
    /// appeared, one of its state keys updated, or it was removed.
    /// Re-read [`DaemonClient::list_tvs`].
    DaemonStateChanged(TvStateChange),
    /// The daemon's liveliness token appeared: it's running and serving
    /// rpcs. Can repeat for one lifetime, so treat it idempotently.
    DaemonAppeared,
    /// The daemon's liveliness token went away (stopped, crashed, or the
    /// link dropped). The mirror keeps its last state.
    DaemonDisappeared,
    /// The session service's liveliness token appeared. Can repeat for
    /// one lifetime.
    SessionAppeared,
    /// The session service's liveliness token went away.
    SessionDisappeared,
    /// A TV's user-profile assignment changed; re-read
    /// [`SessionClient::get_tv_user_profile`] for the TVs shown.
    TvUserProfileChanged,
    /// An existing system profile's body changed.
    SystemProfileChanged(Uuid),
    /// A system profile appeared in the mirror, including each one
    /// replayed when the client first connects.
    SystemProfileAdded(Uuid),
    /// A system profile was deleted.
    SystemProfileRemoved(Uuid),
    /// An existing user profile's body changed.
    UserProfileChanged(Uuid),
    /// A user profile appeared in the mirror, including each one
    /// replayed when the client first connects.
    UserProfileAdded(Uuid),
    /// A user profile was deleted.
    UserProfileRemoved(Uuid),
    /// The session service republished the remote-session fact; re-read
    /// [`SessionClient::remote_session`].
    RemoteSessionChanged,
    /// The tray's liveliness token appeared. Can repeat for one
    /// lifetime.
    TrayAppeared,
    /// The tray's liveliness token went away.
    TrayDisappeared,
    /// The tray published its notification settings, on connect and
    /// after every change; re-read
    /// [`TrayClient::notification_settings`].
    TrayNotificationSettingsChanged,
    /// Re-read everything: the watcher's periodic tick
    /// ([`StateWatcherConfig::fallback_poll_interval`]), also sent when
    /// the watcher fell behind and dropped events.
    FallbackPoll,
    /// A service runs a release this client can't talk to: every rpc to
    /// it fails until it's restarted on a matching release. Sent once per
    /// service lifetime, after its `*Appeared`; a watcher built while the
    /// mismatch is already known gets it on start, and may then see it
    /// twice.
    VersionMismatch {
        /// The mismatched service.
        service: Service,
        /// The protocol release this SDK speaks.
        ours: String,
        /// The service's release.
        theirs: String,
    },
}

/// The tray's notification categories, keyed by their settings field
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotificationCategory {
    /// A TV connected to or disconnected from the daemon.
    TvConnection = 0,
    /// A pairing attempt completed or failed.
    TvPairing = 1,
    /// Wake feedback: waking a TV, then awake or failed.
    TvWake = 2,
    /// The daemon went offline or came back.
    DaemonOffline = 3,
}

impl NotificationCategory {
    /// Every category, in discriminant order.
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

    /// The category whose [`NotificationSettings`] field is named
    /// `key` (`tv_wake`, …); `None` for any other string.
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
#[non_exhaustive]
pub struct TvInfo {
    /// The TV's id, minted by the daemon at pairing; every per-TV call
    /// takes it.
    pub uuid: Uuid,
    /// The user-facing name, set at pairing and by
    /// [`DaemonClient::set_name`].
    pub name: String,
    /// The id of the backend that drives the TV, as in the vendor
    /// catalog's [`VendorEntry::id`].
    pub vendor: String,
    /// The device brand for display; the vendor id when the backend
    /// can't resolve one.
    pub brand: String,
    /// What the backend can do with this TV. A control outside it is
    /// refused with [`ErrorCode::CommandFailed`]. Empty until the TV's
    /// identity is published.
    pub capabilities: Capabilities,
    /// Whether the daemon holds a live connection to the TV. A transport
    /// that stays reachable in standby is connected while the TV is off,
    /// so read `power_state` for power.
    pub connected: bool,
    /// Whether the daemon may run profile-driven actions on the TV. An
    /// unmanaged TV stays connected and still takes manual control.
    pub managed: bool,
    /// Whether profile-driven actions apply right now: the TV is
    /// `managed` and its resolved profile's `automate_when` policy holds
    /// against the current input.
    pub automated: bool,
    /// Whether a remote session currently arms this TV — the read-only
    /// projection of the world's `remote_armed` gate.
    pub remote_armed: bool,
    /// The assigned system profile; `None` uses the default.
    pub system_profile: Option<Uuid>,
    /// The input id (as in `inputs`) a profile action lands the TV on
    /// when it restores the input; empty when none is set.
    pub primary_input: String,
    /// Manufacturer, model and serial as the backend reports them. Each
    /// is `None` until read, and some transports never report one.
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
    /// Which of this machine's display connectors shows the TV, once the
    /// daemon's EDID probe has matched it.
    pub desktop: DesktopMapping,
    /// The TV's key prefix on the bus: every state key it publishes sits
    /// under it, [`TvStateChange::key`] included.
    pub path: String,
    /// The vendor-reported power state; `Unknown` until the TV reports
    /// one, and while the transport can't query it.
    pub power_state: tisvlay_core::PowerState,
    /// Whether the daemon can wake the TV while it's disconnected (for
    /// an IP transport, Wake-on-LAN to a configured MAC). When `false`,
    /// [`DaemonClient::power_on`] on a disconnected TV is refused with
    /// [`ErrorCode::WakeNotSupported`].
    pub supports_wake: bool,
    /// Whether the panel shows a picture: `false` when the TV is off, in
    /// standby or blanked, and before its first report.
    pub is_screen_on: bool,
    /// Volume level, 0–100. Also 0 before the TV reports one, and for a
    /// backend without volume control.
    pub volume: u8,
    /// Whether audio is muted; `false` when unknown.
    pub muted: bool,
    /// The input id showing now, in the id space of `inputs`; empty when
    /// unknown.
    pub current_input: String,
    /// Backlight (panel brightness), 0–100. Also 0 before the TV reports
    /// one, and for a backend without backlight control.
    pub backlight: u8,
    /// The TV's inputs as `(id, label)` pairs. The id is what
    /// [`DaemonClient::switch_input`] and
    /// [`DaemonClient::set_tv_primary_input`] take; the label is for
    /// display. Empty until the TV reports them.
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
    handshake: Arc<Handshake>,
}

impl DaemonClient {
    /// Connect to the daemon socket and start mirroring.
    ///
    /// `site` must be the site the daemon serves. This succeeds whether
    /// or not the daemon is running (see the crate docs); it fails only
    /// with [`Error::Transport`], when the local session or its
    /// subscriptions can't be set up.
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
        let caller = Arc::new(Caller::new(session.clone(), site));
        let handshake = Arc::new(Handshake::new(Service::Daemon));
        {
            let mirror = Arc::clone(&mirror);
            let events = events.clone();
            let caller = Arc::clone(&caller);
            let handshake = Arc::clone(&handshake);
            tokio::spawn(async move {
                while let Ok(sample) = liveliness.recv_async().await {
                    let alive = sample.kind() == SampleKind::Put;
                    {
                        let mut state = mirror.lock().expect("mirror lock");
                        if !alive {
                            state.end_lifetime();
                        }
                        state.daemon_alive = alive;
                    }
                    let _ = events.send(if alive {
                        StateEvent::DaemonAppeared
                    } else {
                        StateEvent::DaemonDisappeared
                    });
                    check_on_liveliness(
                        alive,
                        &handshake,
                        &events,
                        &caller,
                        Service::Daemon,
                        |caller| async move { caller.call::<manager::Info>(&(), &()).await },
                    );
                }
            });
        }
        Ok(Self {
            caller,
            session,
            site: site.to_string(),
            mirror,
            events,
            handshake,
        })
    }

    /// Whether the daemon's liveliness token is present: a mirror read.
    /// `true` doesn't promise the next rpc succeeds (the daemon may be
    /// stopping, or run another release).
    pub async fn is_available(&self) -> bool {
        self.mirror.lock().expect("mirror lock").daemon_alive
    }

    /// Assemble the fleet snapshot the GUI renders from — a pure mirror
    /// projection, no rpcs: the vendor display rows and the primary
    /// descriptor arrive as published state (`meta/vendor`,
    /// `meta/identity`) like everything else.
    pub async fn list_tvs(&self) -> Result<Vec<TvInfo>> {
        self.refuse_known_mismatch()?;
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

    /// The daemon's release. Ungated, so it reports what's running
    /// even across a mismatch.
    pub async fn version(&self) -> Result<String> {
        Ok(self.info().await?.version)
    }

    /// The daemon's state-store path (the zenoh stack has no config
    /// file; the store is what an admin would inspect). Ungated, like
    /// [`Self::version`].
    pub async fn config_path(&self) -> Result<String> {
        Ok(self.info().await?.state_path)
    }

    async fn info(&self) -> Result<rpc::ServiceInfo> {
        self.caller
            .call::<manager::Info>(&(), &())
            .await
            .map_err(|e| Error::from_rpc(Service::Daemon, e))
    }

    /// Apply a hand edit of the daemon's store file: only what changed
    /// since the daemon last wrote it applies, and the report says whether
    /// anything did. Rpc, refused with [`ErrorCode::NoStore`] (the daemon
    /// runs without a file) or [`ErrorCode::StoreUnreadable`] (missing or
    /// unparsable; the message is the parser's report).
    pub async fn reload_config(&self) -> Result<ReloadReport> {
        self.call::<manager::Reload>(&(), &()).await
    }

    /// The daemon's vendor catalog.
    pub async fn get_supported_vendors(&self) -> Result<Vec<VendorEntry>> {
        self.call::<vendors::Catalog>(&(), &()).await
    }

    /// The fields `vendor` asks for at pairing, in display order; each
    /// field's `name` is its key in [`Self::pair_tv`]'s config. Rpc,
    /// refused with [`ErrorCode::UnknownVendor`].
    pub async fn get_pairing_schema(&self, vendor: &str) -> Result<Vec<PairingField>> {
        self.call::<vendors::Schema>(vendor, &()).await
    }

    /// Start pairing a new TV named `name` on backend `vendor`, from the
    /// vendor config `config` (keyed by the pairing schema's field
    /// names). `managed` sets [`TvInfo::managed`] for the enrolled TV.
    /// For an IP-addressed TV, the daemon fills a missing MAC from its
    /// ARP cache.
    ///
    /// The reply comes once pairing has started, not finished: it
    /// carries the new TV's id and the attempt id. Follow the attempt
    /// with [`Self::pairing_progress_stream`] and
    /// [`Self::drain_pair_to_terminal`]; the TV is enrolled only on
    /// [`PairTerminal::Complete`]. Rpc, refused with
    /// [`ErrorCode::InvalidField`] (a required field missing or blank,
    /// see [`Error::invalid_field`]), [`ErrorCode::BadRequest`],
    /// [`ErrorCode::UnknownVendor`] or [`ErrorCode::PairingExists`].
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
    /// Rpc, refused with [`ErrorCode::TvNotFound`] or
    /// [`ErrorCode::PairingExists`] (one is already running for the TV).
    pub async fn re_pair(&self, tv: Uuid) -> Result<Uuid> {
        let started = self.call::<tv::RePair>(&tv, &()).await?;
        Ok(started.attempt)
    }

    /// Ask the daemon to cancel the pairing session running for `tv` (a
    /// new TV's id from [`Self::pair_tv`], or a re-paired TV). The ack
    /// means the request was taken; the attempt's end arrives on the
    /// progress stream, normally `failed` with
    /// [`FailureCode::PairingCancelled`]. Rpc, refused with
    /// [`ErrorCode::PairingNotFound`].
    pub async fn cancel_pair(&self, tv: Uuid) -> Result<()> {
        self.call::<manager::CancelPair>(&(), &rpc::TvRef { id: tv })
            .await
            .map(|_| ())
    }

    /// Every pairing session's progress samples as `(tv, sample)`,
    /// hydrated from the deep pairing caches so a re-opened wizard
    /// replays prior phases. Refused with [`Error::VersionMismatch`] while
    /// the daemon is known to run another release, and silent while it
    /// does.
    pub async fn pairing_progress_stream(
        &self,
    ) -> Result<impl futures_util::Stream<Item = (Uuid, PairingProgress)> + use<>> {
        self.refuse_known_mismatch()?;
        let sub = self
            .session
            .declare_subscriber(keys::pairing_progress_wild(&self.site))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| Error::transport("subscribe to pairing progress", e))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let site = self.site.clone();
        let handshake = Arc::clone(&self.handshake);
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                if handshake.known_mismatch().is_some() {
                    continue;
                }
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

    /// One TV's `progress/power` phases (wake feedback). Refused with
    /// [`Error::VersionMismatch`] while the daemon is known to run another
    /// release, and silent while it does.
    pub async fn power_cycle_progress_stream(
        &self,
        tv: Uuid,
    ) -> Result<impl futures_util::Stream<Item = PowerPhase> + use<>> {
        self.refuse_known_mismatch()?;
        let sub = self
            .session
            .declare_subscriber(keys::tv_path(&self.site, &tv, "progress/power"))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .map_err(|e| Error::transport("subscribe to power progress", e))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let handshake = Arc::clone(&self.handshake);
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                if handshake.known_mismatch().is_some() {
                    continue;
                }
                let Ok(progress) = serde_json::from_slice::<tisvlay_protocol::payload::PowerProgress>(
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
    /// cadence until [`Self::stop_discovery`]. Rpc; it scans every vendor,
    /// so it isn't refused.
    pub async fn start_discovery(&self) -> Result<Uuid> {
        let started = self
            .call::<manager::DiscoverStart>(&(), &rpc::DiscoverStart { vendor: None })
            .await?;
        Ok(started.session)
    }

    /// Close the discovery session [`Self::start_discovery`] returned.
    /// Rpc, refused with [`ErrorCode::DiscoveryNotFound`].
    pub async fn stop_discovery(&self, session: Uuid) -> Result<()> {
        self.call::<manager::DiscoverStop>(&(), &rpc::DiscoverStop { session })
            .await
            .map(|_| ())
    }

    /// Every discovery session's findings. Refused with
    /// [`Error::VersionMismatch`] while the daemon is known to run another
    /// release, and silent while it does.
    pub async fn discovery_found_stream(
        &self,
    ) -> Result<impl futures_util::Stream<Item = DiscoveredTv> + use<>> {
        self.refuse_known_mismatch()?;
        let sub = self
            .session
            .declare_subscriber(keys::discovery_found_wild(&self.site))
            .await
            .map_err(|e| Error::transport("subscribe to discovery findings", e))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let handshake = Arc::clone(&self.handshake);
        tokio::spawn(async move {
            while let Ok(sample) = sub.recv_async().await {
                if handshake.known_mismatch().is_some() {
                    continue;
                }
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
    /// safety net. Rpc, refused with [`ErrorCode::UnknownVendor`].
    pub async fn enrich_manual_entry(&self, vendor: &str, address: &str) -> Result<Option<String>> {
        let request = rpc::Enrich {
            vendor: vendor.into(),
            ip: address.into(),
        };
        Ok(self.call::<manager::Enrich>(&(), &request).await?.mac)
    }

    /// Turn the TV on: in-band over a live connection, otherwise by
    /// waking it and reconnecting. The ack comes once the command is
    /// sent or the wake is under way, not once the TV is on; follow a
    /// wake with [`Self::power_cycle_progress_stream`]. Rpc, refused
    /// with [`ErrorCode::TvNotFound`], [`ErrorCode::NotConnected`] or
    /// [`ErrorCode::WakeNotSupported`] (disconnected, and the transport
    /// can't wake it; see [`TvInfo::supports_wake`]).
    pub async fn power_on(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::PowerOn>(tv, &rpc::PowerOn::default())
            .await
    }

    /// Put the TV in standby. Acks once the TV has carried the command
    /// out. Rpc, refused with [`ErrorCode::TvNotFound`],
    /// [`ErrorCode::NotConnected`] or [`ErrorCode::CommandFailed`] (the
    /// TV refused or failed it).
    pub async fn power_off(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::PowerOff>(tv, &()).await
    }

    /// Light the panel. On a disconnected TV that can be woken, or one
    /// half asleep, this wakes it instead of failing. The ack comes once
    /// the action is sent or the wake is under way; the daemon then
    /// checks the power state and retries on its own. Rpc, refused with
    /// [`ErrorCode::TvNotFound`] or [`ErrorCode::NotConnected`]
    /// (disconnected, and the transport can't wake it).
    pub async fn screen_on(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::SetScreen>(tv, &rpc::SetScreen::manual(true))
            .await
    }

    /// Blank the panel without powering the TV off. A disconnected TV
    /// already counts as dark, so that succeeds without doing anything.
    /// The ack comes once the action is sent; the daemon then checks the
    /// power state and retries on its own. Rpc, refused with
    /// [`ErrorCode::TvNotFound`], or [`ErrorCode::NotConnected`] when the
    /// connection drops as the action is sent.
    pub async fn screen_off(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::SetScreen>(tv, &rpc::SetScreen::manual(false))
            .await
    }

    /// Switch to `input`, an id from [`TvInfo::inputs`]. A manual switch
    /// wins over a pending profile restore of the primary input, which
    /// it cancels. Acks once the TV has switched. Rpc, refused with
    /// [`ErrorCode::TvNotFound`], [`ErrorCode::NotConnected`] or
    /// [`ErrorCode::CommandFailed`].
    pub async fn switch_input(&self, tv: Uuid, input: &str) -> Result<()> {
        self.tv_call::<tv::SwitchInput>(
            tv,
            &rpc::SwitchInput {
                input: input.into(),
            },
        )
        .await
    }

    /// Set the volume to `level`, 0–100. Acks once the TV has applied
    /// it. Rpc, refused with [`ErrorCode::TvNotFound`],
    /// [`ErrorCode::NotConnected`] or [`ErrorCode::CommandFailed`]
    /// (including a backend without volume control).
    pub async fn set_volume(&self, tv: Uuid, level: u8) -> Result<()> {
        self.tv_call::<tv::SetVolume>(tv, &rpc::SetVolume { level })
            .await
    }

    /// Mute or unmute the TV. Acks once the TV has applied it. Rpc,
    /// refused with [`ErrorCode::TvNotFound`],
    /// [`ErrorCode::NotConnected`] or [`ErrorCode::CommandFailed`].
    pub async fn set_muted(&self, tv: Uuid, muted: bool) -> Result<()> {
        self.tv_call::<tv::SetMuted>(tv, &rpc::SetMuted { muted })
            .await
    }

    /// Set the backlight (panel brightness) to `level`, 0–100. Acks once
    /// the TV has applied it. Rpc, refused with
    /// [`ErrorCode::TvNotFound`], [`ErrorCode::NotConnected`] or
    /// [`ErrorCode::CommandFailed`].
    pub async fn set_backlight(&self, tv: Uuid, level: u8) -> Result<()> {
        self.tv_call::<tv::SetBacklight>(tv, &rpc::SetBacklight { level })
            .await
    }

    /// Rename the TV ([`TvInfo::name`]); the TV itself isn't told. Rpc,
    /// refused with [`ErrorCode::TvNotFound`].
    pub async fn set_name(&self, tv: Uuid, name: &str) -> Result<()> {
        self.tv_call::<tv::SetName>(tv, &rpc::SetName { name: name.into() })
            .await
    }

    /// Set [`TvInfo::managed`]. Managing a TV that has no connection
    /// makes the daemon dial it; unmanaging one keeps its connection and
    /// manual control, and stops only profile-driven actions. Rpc,
    /// refused with [`ErrorCode::TvNotFound`].
    pub async fn set_tv_managed(&self, tv: Uuid, managed: bool) -> Result<()> {
        self.tv_call::<tv::SetManaged>(tv, &rpc::SetManaged { managed })
            .await
    }

    /// Set the TV's primary input to an id from [`TvInfo::inputs`], or
    /// clear it with `None`. Profile actions that restore the input land
    /// the TV there, and an `automate_when` policy keyed on the primary
    /// input compares against it. Rpc, refused with
    /// [`ErrorCode::TvNotFound`].
    pub async fn set_tv_primary_input(&self, tv: Uuid, input: Option<&str>) -> Result<()> {
        let input = input.map(str::to_string);
        self.tv_call::<tv::SetPrimaryInput>(tv, &rpc::SetPrimaryInput { input })
            .await
    }

    /// Assign a system profile to the TV, or `None` for the default
    /// profile. Rpc, refused with [`ErrorCode::TvNotFound`] or
    /// [`ErrorCode::ProfileNotFound`] (no system profile has that id).
    pub async fn set_tv_system_profile(&self, tv: Uuid, profile: Option<Uuid>) -> Result<()> {
        self.tv_call::<tv::SetSystemProfile>(tv, &rpc::SetSystemProfile { profile })
            .await
    }

    /// Dial the TV now, skipping any retry backoff. Does nothing when
    /// it's connected or a dial is already in flight; the ack doesn't
    /// wait for the connection. Rpc, refused with
    /// [`ErrorCode::TvNotFound`].
    pub async fn reconnect(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::Reconnect>(tv, &()).await
    }

    /// Forget the TV: the daemon drops its connection and stored config,
    /// cancels a re-pair in flight, and deletes its published state,
    /// which surfaces as [`StateEvent::DaemonStateChanged`]. Rpc,
    /// refused with [`ErrorCode::TvNotFound`].
    pub async fn remove_tv(&self, tv: Uuid) -> Result<()> {
        self.tv_call::<tv::Remove>(tv, &()).await
    }

    /// The TV's config, schema-secret fields removed. Rpc, refused with
    /// [`ErrorCode::TvNotFound`].
    pub async fn get_tv_config(&self, tv: Uuid) -> Result<TvConfigView> {
        self.call::<tv::GetConfig>(&tv, &()).await
    }

    /// Patch the TV's config: known top-level keys apply to its settings,
    /// everything else merges into the vendor table. Rpc, refused with
    /// [`ErrorCode::TvNotFound`], [`ErrorCode::InvalidField`] (a value the
    /// field rejects; see [`Error::invalid_field`]) or
    /// [`ErrorCode::BadRequest`] (a key of the wrong type).
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
        if self.handshake.known_mismatch().is_some() {
            return None;
        }
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
    }

    /// Every system profile by id, the reserved default included: a
    /// mirror read. Empty once the daemon is known to run a mismatched
    /// release.
    pub async fn list_system_profile_bodies(&self) -> HashMap<Uuid, SystemProfileBody> {
        if self.handshake.known_mismatch().is_some() {
            return HashMap::new();
        }
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .iter()
            .map(|(id, body)| (*id, body.clone()))
            .collect()
    }

    /// Create a system profile named `name` with every field unset, so
    /// it inherits the default profile throughout; returns its id. Rpc.
    pub async fn create_system_profile(&self, name: &str) -> Result<Uuid> {
        let body = SystemProfileBody {
            name: name.into(),
            ..Default::default()
        };
        let request = rpc::ProfileCreate { id: None, body };
        Ok(self.call::<profile::Create>(&(), &request).await?.id)
    }

    /// Replace a system profile's whole body with `body`. Rpc, refused
    /// with [`ErrorCode::ProfileNotFound`], or
    /// [`ErrorCode::BadRequest`] when renaming the reserved default.
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

    /// Delete a system profile. Rpc, refused with
    /// [`ErrorCode::ProfileNotFound`], [`ErrorCode::ProfileReserved`]
    /// (the default profile) or [`ErrorCode::ProfileInUse`] (a TV is
    /// assigned to it).
    pub async fn delete_system_profile(&self, profile: Uuid) -> Result<()> {
        self.call::<profile::Delete>(&(), &rpc::TvRef { id: profile })
            .await
            .map(|_| ())
    }

    /// Copy a system profile's body under the name `name`; returns the
    /// copy's id. Rpc, refused with [`ErrorCode::ProfileNotFound`].
    pub async fn duplicate_system_profile(&self, profile: Uuid, name: &str) -> Result<Uuid> {
        let request = rpc::ProfileDuplicate {
            id: profile,
            name: name.into(),
        };
        Ok(self.call::<profile::Duplicate>(&(), &request).await?.id)
    }

    /// The built-in fallback values every system-profile field resolves
    /// to when no profile sets it, named `default`. These are the stock
    /// values, not the editable default profile. Rpc.
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
        self.handshake
            .ensure(&self.events, || async { Ok(self.info().await?.version) })
            .await?;
        self.caller
            .call::<R>(target, request)
            .await
            .map_err(|e| Error::from_rpc(Service::Daemon, e))
    }

    /// Fail when the daemon is already known to run another release: the
    /// gate the mirror reads and progress streams share, which doesn't ask.
    fn refuse_known_mismatch(&self) -> Result<()> {
        match self.handshake.known_mismatch() {
            Some(mismatch) => Err(mismatch),
            None => Ok(()),
        }
    }
}

/// Client for the per-user session service (user profiles).
#[derive(Debug, Clone)]
pub struct SessionClient {
    caller: Arc<Caller>,
    mirror: Arc<Mutex<UserMirror>>,
    events: broadcast::Sender<StateEvent>,
    handshake: Arc<Handshake>,
}

impl SessionClient {
    /// Connect to the user socket the session service serves and start
    /// mirroring its user profiles, assignments and remote-session fact.
    /// Like [`DaemonClient::connect`], this doesn't need the service
    /// running and fails only with [`Error::Transport`].
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
        let caller = Arc::new(Caller::new(session, site));
        let handshake = Arc::new(Handshake::new(Service::Session));
        {
            let mirror = Arc::clone(&mirror);
            let events = events.clone();
            let caller = Arc::clone(&caller);
            let handshake = Arc::clone(&handshake);
            tokio::spawn(async move {
                while let Ok(sample) = liveliness.recv_async().await {
                    let alive = sample.kind() == SampleKind::Put;
                    mirror.lock().expect("mirror lock").session_alive = alive;
                    let _ = events.send(if alive {
                        StateEvent::SessionAppeared
                    } else {
                        StateEvent::SessionDisappeared
                    });
                    check_on_liveliness(
                        alive,
                        &handshake,
                        &events,
                        &caller,
                        Service::Session,
                        |caller| async move { caller.call::<user_profile::Info>(&(), &()).await },
                    );
                }
            });
        }
        Ok(Self {
            caller,
            mirror,
            events,
            handshake,
        })
    }

    /// Whether the session service's liveliness token is present: a
    /// mirror read, with the same caveat as
    /// [`DaemonClient::is_available`].
    pub async fn is_available(&self) -> bool {
        self.mirror.lock().expect("mirror lock").session_alive
    }

    /// The session service's release. Ungated, like the daemon's and the
    /// tray's, so it reports what's running even across a mismatch.
    pub async fn version(&self) -> Result<String> {
        Ok(self.info().await?.version)
    }

    /// The session service's state-file path; empty when it runs without
    /// one. Ungated, like [`Self::version`].
    pub async fn config_path(&self) -> Result<String> {
        Ok(self.info().await?.state_path)
    }

    /// Apply a hand edit of the session's store file (user profiles and
    /// assignments): only what changed since the session last wrote it
    /// applies. Rpc, refused with [`ErrorCode::NoStore`] (the session
    /// runs without a file) or [`ErrorCode::StoreUnreadable`] (missing or
    /// unparsable; the message is the parser's report).
    pub async fn reload_config(&self) -> Result<ReloadReport> {
        self.call::<user_profile::Reload>(&(), &()).await
    }

    /// The TV's assigned user-profile uuid, from the mirror; `None` when
    /// it uses the default.
    pub async fn get_tv_user_profile(&self, tv: Uuid) -> Option<Uuid> {
        if self.handshake.known_mismatch().is_some() {
            return None;
        }
        self.mirror
            .lock()
            .expect("mirror lock")
            .assignments
            .get(&tv)
            .copied()
    }

    /// Assign a user profile to `tv`, or `None` to fall back to the
    /// default. The TV id isn't checked against the daemon. Rpc, refused
    /// with [`ErrorCode::ProfileNotFound`].
    pub async fn set_tv_user_profile(&self, tv: Uuid, profile: Option<Uuid>) -> Result<()> {
        self.call::<user_profile::Assign>(&(), &rpc::UserAssign { tv, profile })
            .await
            .map(|_| ())
    }

    /// Every user profile by id, the reserved default included: a mirror
    /// read. Empty once the session service is known to run a
    /// mismatched release.
    pub async fn list_user_profile_bodies(&self) -> HashMap<Uuid, UserProfileBody> {
        if self.handshake.known_mismatch().is_some() {
            return HashMap::new();
        }
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .iter()
            .map(|(id, body)| (*id, body.clone()))
            .collect()
    }

    /// A user profile's body, from the mirror; `None` against a service
    /// on another release.
    pub async fn get_user_profile(&self, profile: Uuid) -> Option<UserProfileBody> {
        if self.handshake.known_mismatch().is_some() {
            return None;
        }
        self.mirror
            .lock()
            .expect("mirror lock")
            .profiles
            .get(&profile)
            .cloned()
    }

    /// Create a user profile named `name` with every field unset, so it
    /// inherits the default profile throughout; returns its id. Rpc.
    pub async fn create_user_profile(&self, name: &str) -> Result<Uuid> {
        let body = UserProfileBody {
            name: name.into(),
            ..Default::default()
        };
        let request = rpc::UserProfileCreate { id: None, body };
        Ok(self.call::<user_profile::Create>(&(), &request).await?.id)
    }

    /// Replace a user profile's whole body with `body`. Rpc, refused
    /// with [`ErrorCode::ProfileNotFound`], or [`ErrorCode::BadRequest`]
    /// when renaming the reserved default.
    pub async fn update_user_profile(&self, profile: Uuid, body: UserProfileBody) -> Result<()> {
        let request = rpc::UserProfileUpdate { id: profile, body };
        self.call::<user_profile::Update>(&(), &request)
            .await
            .map(|_| ())
    }

    /// Delete a user profile. Rpc, refused with
    /// [`ErrorCode::ProfileReserved`] (the default profile),
    /// [`ErrorCode::ProfileNotFound`] or [`ErrorCode::ProfileInUse`]
    /// (assigned to a TV).
    pub async fn delete_user_profile(&self, profile: Uuid) -> Result<()> {
        self.call::<user_profile::Delete>(&(), &rpc::TvRef { id: profile })
            .await
            .map(|_| ())
    }

    /// Copy a user profile's body under the name `name`; returns the
    /// copy's id. Rpc, refused with [`ErrorCode::ProfileNotFound`].
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

    /// The built-in values a user-profile field falls back to when
    /// neither its profile nor the default profile sets it, as a body
    /// with every field set, named `default`. Rpc.
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
        if self.handshake.known_mismatch().is_some() {
            return None;
        }
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
        self.handshake
            .ensure(&self.events, || async { Ok(self.info().await?.version) })
            .await?;
        self.caller
            .call::<R>(target, request)
            .await
            .map_err(|e| Error::from_rpc(Service::Session, e))
    }

    async fn info(&self) -> Result<rpc::ServiceInfo> {
        self.caller
            .call::<user_profile::Info>(&(), &())
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
    handshake: Arc<Handshake>,
}

impl TrayClient {
    /// Connect to the user socket the tray's settings service is on (the
    /// session service's). Like [`DaemonClient::connect`], this doesn't
    /// need the tray running and fails only with [`Error::Transport`].
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
        let caller = Arc::new(Caller::new(session.clone(), site));
        let handshake = Arc::new(Handshake::new(Service::Tray));
        {
            let alive = Arc::clone(&alive);
            let events = events.clone();
            let caller = Arc::clone(&caller);
            let handshake = Arc::clone(&handshake);
            tokio::spawn(async move {
                while let Ok(sample) = liveliness.recv_async().await {
                    let up = sample.kind() == SampleKind::Put;
                    *alive.lock().expect("tray alive lock") = up;
                    let _ = events.send(if up {
                        StateEvent::TrayAppeared
                    } else {
                        StateEvent::TrayDisappeared
                    });
                    check_on_liveliness(
                        up,
                        &handshake,
                        &events,
                        &caller,
                        Service::Tray,
                        |caller| async move { caller.call::<user_tray::Info>(&(), &()).await },
                    );
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
            caller,
            alive,
            events,
            handshake,
        })
    }

    /// Whether the tray's liveliness token is present, with the same
    /// caveat as [`DaemonClient::is_available`]. No rpc.
    pub async fn is_available(&self) -> bool {
        *self.alive.lock().expect("tray alive lock")
    }

    /// The tray's release. Ungated, so it reports what's running even
    /// across a mismatch.
    pub async fn version(&self) -> Result<String> {
        Ok(self.info().await?.version)
    }

    /// The tray's settings-file path. Ungated, like [`Self::version`].
    pub async fn config_path(&self) -> Result<String> {
        Ok(self.info().await?.state_path)
    }

    /// Apply a hand edit of the tray's settings file. Rpc, refused with
    /// [`ErrorCode::StoreUnreadable`] (missing or unparsable; the message
    /// is the parser's report).
    pub async fn reload_config(&self) -> Result<ReloadReport> {
        self.call::<user_tray::Reload>(&()).await
    }

    /// The tray's notification switches, one per
    /// [`NotificationCategory`]. Rpc; the client keeps no copy, so
    /// re-read on [`StateEvent::TrayNotificationSettingsChanged`].
    pub async fn notification_settings(&self) -> Result<NotificationSettings> {
        self.call::<user_tray::Get>(&()).await
    }

    /// Change the switches `patch` sets, leaving the rest as they are.
    /// The tray saves them to its settings file and republishes them.
    /// Rpc.
    pub async fn update_notification_settings(
        &self,
        patch: NotificationSettingsPatch,
    ) -> Result<()> {
        self.call::<user_tray::Update>(&patch).await.map(|_| ())
    }

    /// Have the tray show a test desktop notification, whatever the
    /// switches say. Rpc.
    pub async fn send_test_notification(&self) -> Result<()> {
        self.call::<user_tray::Test>(&()).await.map(|_| ())
    }

    fn subscribe_events(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    async fn call<R: Unary<Target = ()>>(&self, request: &R::Request) -> Result<R::Response> {
        self.handshake
            .ensure(&self.events, || async { Ok(self.info().await?.version) })
            .await?;
        self.caller
            .call::<R>(&(), request)
            .await
            .map_err(|e| Error::from_rpc(Service::Tray, e))
    }

    async fn info(&self) -> Result<rpc::ServiceInfo> {
        self.caller
            .call::<user_tray::Info>(&(), &())
            .await
            .map_err(|e| Error::from_rpc(Service::Tray, e))
    }
}

/// Keep a client's verdict in step with its service's liveliness: a
/// service that went away gets a fresh check in its next lifetime, and one
/// that appeared is checked at once, so a mismatch reaches the watcher
/// without waiting for a call. The check runs on its own task, so a slow
/// `info` never holds up the next liveliness edge.
fn check_on_liveliness<F, Fut>(
    alive: bool,
    handshake: &Arc<Handshake>,
    events: &broadcast::Sender<StateEvent>,
    caller: &Arc<Caller>,
    service: Service,
    info: F,
) where
    F: FnOnce(Arc<Caller>) -> Fut + Send + 'static,
    Fut: Future<Output = std::result::Result<rpc::ServiceInfo, tisvlay_protocol::caller::RpcError>>
        + Send,
{
    if !alive {
        handshake.reset();
        return;
    }
    let handshake = Arc::clone(handshake);
    let events = events.clone();
    let caller = Arc::clone(caller);
    tokio::spawn(async move {
        let _ = handshake
            .ensure(&events, || async move {
                info(caller)
                    .await
                    .map(|info| info.version)
                    .map_err(|e| Error::from_rpc(service, e))
            })
            .await;
    });
}

/// Watcher tuning: mirror deltas forward directly, so the one knob is
/// the fallback poll interval.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StateWatcherConfig {
    /// How often the watcher emits [`StateEvent::FallbackPoll`], a
    /// safety net for a missed change. Must be nonzero: a zero interval
    /// panics the watcher's poll task, and no poll fires.
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
    /// Merge the events of `daemon` and of the optional `session` and
    /// `tray` clients into one stream. Each service already alive when
    /// the watcher is built gets an `*Appeared` event, so a consumer
    /// driven by events alone still learns of it. Must run inside the
    /// tokio runtime.
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
        // Synthesize `*Appeared` for peers already alive, and
        // `VersionMismatch` for mismatches already known: both fired into
        // the client's broadcast before this watcher subscribed (a tokio
        // broadcast drops events sent with no receivers), so consumers
        // relying on edges alone would never learn of them. A
        // level-triggered emit after subscription closes the window; a
        // check finishing in between can announce twice, and consumers
        // treat repeats idempotently.
        {
            let tx = tx.clone();
            let daemon = daemon.clone();
            let session = session.cloned();
            let tray = tray.cloned();
            tokio::spawn(async move {
                if daemon.is_available().await {
                    let _ = tx.send(StateEvent::DaemonAppeared);
                }
                if let Some(event) = daemon.handshake.known_mismatch_event() {
                    let _ = tx.send(event);
                }
                if let Some(session) = session {
                    if session.is_available().await {
                        let _ = tx.send(StateEvent::SessionAppeared);
                    }
                    if let Some(event) = session.handshake.known_mismatch_event() {
                        let _ = tx.send(event);
                    }
                }
                if let Some(tray) = tray {
                    if tray.is_available().await {
                        let _ = tx.send(StateEvent::TrayAppeared);
                    }
                    if let Some(event) = tray.handshake.known_mismatch_event() {
                        let _ = tx.send(event);
                    }
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

    /// The next event, waiting for one. `None` only once the watcher's
    /// tasks have stopped, which happens when the runtime shuts down.
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
    use tisvlay_protocol::payload;

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
