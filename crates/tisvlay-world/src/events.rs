//! Inbound events, commands, and outbound effects — the world's entire I/O
//! vocabulary.

use bevy_ecs::message::Message;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::marker::PhantomData;
use std::time::Instant;
use tisvlay_core::{TvState, VendorConfig};
use tisvlay_protocol::endpoint::{Rpc, manager, profile, tv};
use tisvlay_protocol::rpc::Ack;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Correlation id for a command reply — the bridge maps it back onto the
/// zenoh query that asked.
pub type ReplyTo = u64;

/// A reply owed to one rpc caller, typed by the rpc it answers: replying
/// takes that rpc's response type, so a mismatched reply doesn't compile.
///
/// ```
/// # use tisvlay_world::Reply;
/// # use tisvlay_protocol::{endpoint::manager, rpc};
/// let _ = Reply::<manager::AddTv>::new(1).ok(&rpc::Created { id: uuid::Uuid::nil() });
/// ```
///
/// ```compile_fail,E0308
/// # use tisvlay_world::Reply;
/// # use tisvlay_protocol::{endpoint::manager, rpc};
/// let _ = Reply::<manager::AddTv>::new(1).ok(&rpc::Ack {});
/// ```
///
/// Only an rpc answered by [`Ack`] erases into an [`AckReply`]:
///
/// ```
/// # use tisvlay_world::{AckReply, Reply};
/// # use tisvlay_protocol::endpoint::tv;
/// let _ = AckReply::from(Reply::<tv::SetVolume>::new(1));
/// ```
///
/// ```compile_fail,E0277
/// # use tisvlay_world::{AckReply, Reply};
/// # use tisvlay_protocol::endpoint::manager;
/// let _ = AckReply::from(Reply::<manager::AddTv>::new(1));
/// ```
pub struct Reply<R> {
    to: ReplyTo,
    rpc: PhantomData<fn() -> R>,
}

impl<R> Clone for Reply<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for Reply<R> {}

impl<R> fmt::Debug for Reply<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Reply").field(&self.to).finish()
    }
}

impl<R> PartialEq for Reply<R> {
    fn eq(&self, other: &Self) -> bool {
        self.to == other.to
    }
}

impl<R: Rpc> Reply<R> {
    /// The reply to the query the bridge filed under `to`.
    pub fn new(to: ReplyTo) -> Self {
        Self {
            to,
            rpc: PhantomData,
        }
    }

    /// The effect answering the caller with `response`.
    pub fn ok(self, response: &R::Response) -> WireEffect {
        WireEffect::Reply {
            to: self.to,
            result: Ok(serde_json::to_value(response)
                .expect("rpc responses are serde structs, string-keyed maps and lists of them, which always encode as JSON")),
        }
    }
}

/// A reply known to answer with [`Ack`]: what the paths several
/// rpcs share (power and screen dispatch) and the backend's deferred acks
/// carry, without naming the rpc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckReply(ReplyTo);

impl<R: Rpc<Response = Ack>> From<Reply<R>> for AckReply {
    fn from(reply: Reply<R>) -> Self {
        Self(reply.to)
    }
}

impl AckReply {
    /// The effect acknowledging the caller.
    pub fn ok(self) -> WireEffect {
        WireEffect::Reply {
            to: self.0,
            result: Ok(serde_json::json!({})),
        }
    }
}

/// Any reply, erased: refusals carry no response type, so every reply can
/// answer with an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnyReply(ReplyTo);

impl<R> From<Reply<R>> for AnyReply {
    fn from(reply: Reply<R>) -> Self {
        Self(reply.to)
    }
}

impl From<AckReply> for AnyReply {
    fn from(reply: AckReply) -> Self {
        Self(reply.0)
    }
}

impl AnyReply {
    /// The effect refusing the caller's request.
    pub fn err(self, error: WireError) -> WireEffect {
        WireEffect::Reply {
            to: self.0,
            result: Err(error),
        }
    }
}

/// Everything that can enter the world through the reactor inbox.
#[derive(Message, Debug)]
pub enum InEvent {
    /// A control-plane command (RPC via the bridge, or a test driver).
    Command(Command),
    /// A fact reported by a backend connection task, tagged with the
    /// dial that task was spawned for — events from a superseded dial
    /// are dropped.
    Backend {
        tv: Uuid,
        dial: u64,
        event: BackendEvent,
    },
    /// A fact reported by a pairing task (or the reactor's pairing glue).
    PairingUpdate { id: Uuid, update: PairingUpdate },
    /// A desktop/power event from an adapter (logind, DPMS, lock),
    /// dispatched through the system profile to per-TV actions. A
    /// `connector` scopes the event to the TV mirroring that DRM
    /// connector; TVs without a mapping treat every event as global.
    System {
        event: SystemEvent,
        connector: Option<String>,
    },
    /// The daemon's logind adapter couldn't put the machine to sleep for a
    /// TV's power-off (polkit refused it, an inhibitor blocked it, or the
    /// host has no such mode): published as `event/dispatch-failed`.
    SleepRefused {
        tv: Uuid,
        mode: SleepMode,
        message: String,
    },
    /// Queue the token on every live connection behind the commands
    /// already queued there. The reactor fans it out after its step's
    /// dispatch, so it also covers a `System` event sent just before it.
    Drain(DrainToken),
    /// The mirrored stream fact: the set of TVs the session
    /// service computed as armed (a stream is active and the
    /// resolved user profile arms it). Consumed into the `StreamArmed`
    /// resource; an empty set disarms every TV. The world sees only the
    /// armed projection — arming policy stays session-side.
    Stream { armed: Vec<Uuid> },
    /// NIC carrier transition — parks/unparks connections whose
    /// monitored interface (route-resolved NIC) matches `interface`.
    Carrier { interface: String, up: bool },
    /// Route resolution outcome for a TV: the NIC carrying traffic to
    /// its address (`ip route get`), or `None` when no usable route
    /// exists. Sent by the daemon's netlink monitor at monitor-request
    /// time and whenever the kernel routing table changes.
    RouteResolved { tv: Uuid, interface: Option<String> },
    /// Whether a TV's local device node (its `TvConnector::local_device`)
    /// is usable: present, and initialized by udev where udev runs. Sent
    /// by the daemon's device watcher at monitor-request time and
    /// whenever udev reports a device coming or going.
    DeviceAvailability { tv: Uuid, available: bool },
    /// The desktop probe matched a TV's hardware identity to a DRM
    /// connector.
    ConnectorMatched { tv: Uuid, connector: String },
    /// Config-derived connector statics for a TV, evaluated by the
    /// reactor (which owns the connectors) at enrollment and on every
    /// dial spawn — present before any dial outcome exists.
    TransportFacts {
        tv: Uuid,
        supports_wake: bool,
        traits: TransportInfo,
        /// The connector's primary identifying descriptor (LG: IP; CEC:
        /// device node) — published on the TV's identity.
        primary: Option<String>,
        /// Schema-ordered, labeled, Secret-free display rows
        /// (`display_config` / `display_diagnostics`) — published as
        /// `VendorRows` for the clients' INFO surface.
        config_rows: Vec<(String, String)>,
        diagnostic_rows: Vec<(String, String)>,
        /// The local device node the connection needs (CEC's `/dev/cecN`,
        /// as configured), for the wake list.
        device: Option<std::path::PathBuf>,
    },
    /// Time advance. Consumed by the reactor itself (updates the world
    /// clock); never dispatched as a bevy message.
    Tick { now: Instant },
}

/// Requests from the reactor to the daemon's netlink monitor: which TV
/// addresses to route-resolve and watch. The reactor emits `Monitor` for
/// every network-dependent TV at enrollment and on each dial spawn, and
/// `Forget` on removal; the monitor answers (and keeps answering, on
/// routing-table changes) with [`InEvent::RouteResolved`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteRequest {
    Monitor { tv: Uuid, host: String },
    Forget { tv: Uuid },
}

/// Requests from the reactor to the daemon's device watcher: which TVs'
/// local device nodes to watch. The reactor emits `Monitor` for every TV
/// whose connector names a local device, at enrollment and on each dial
/// spawn, and `Forget` on removal; the watcher answers (and keeps
/// answering, on udev events) with [`InEvent::DeviceAvailability`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceRequest {
    Monitor { tv: Uuid, path: std::path::PathBuf },
    Forget { tv: Uuid },
}

/// Desktop/power events the profile dispatcher maps to TV actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemEvent {
    /// Daemon boot — the WoL-at-boot event (`on_startup`, stock
    /// `power_on`). Dispatched once by the daemon after store replay.
    Startup,
    Sleep,
    Resume,
    Shutdown,
    /// Shutdown that logind reports as a reboot/kexec — `on_reboot`
    /// (stock `none`: the TV stays on across a quick reboot).
    Reboot,
    DpmsOff,
    DpmsOn,
    /// Login greeter went idle (published by `tisvlay-greeter`).
    GreeterIdle,
    /// Login greeter activity resumed.
    GreeterResume,
}

/// Updates flowing from a pairing session into the world.
#[derive(Debug)]
pub enum PairingUpdate {
    /// The session's cancel channel, sent once at task start.
    Handle(mpsc::Sender<tisvlay_core::PairingCommand>),
    /// A pairing state transition (progress or terminal).
    State(Box<tisvlay_core::PairingState>),
    /// The reactor built the final vendor config from credentials — spawn
    /// the TV (or update it, for a re-pair).
    ConfigReady(VendorConfig),
    /// Config build failed after successful pairing.
    ConfigFailed(String),
}

#[derive(Debug)]
pub enum Command {
    /// Pair a new TV. Async-spawn semantics: the reply acknowledges
    /// validation; progress streams on `pairing/<uuid>/progress` and the
    /// TV's keys appear once the handshake completes.
    Pair {
        id: Uuid,
        /// The attempt id stamped on this session's progress samples.
        attempt: Uuid,
        name: String,
        vendor: String,
        config: VendorConfig,
        managed: bool,
        reply: Option<Reply<manager::Pair>>,
    },
    /// Re-run the pairing handshake for an existing TV (refreshes
    /// credentials); progress streams on the TV's pairing key.
    RePair {
        id: Uuid,
        attempt: Uuid,
        reply: Option<Reply<tv::RePair>>,
    },
    CancelPair {
        id: Uuid,
        reply: Option<Reply<manager::CancelPair>>,
    },
    /// Enroll a TV directly from a full store row — the boot-replay and
    /// rpc enrollment path alike.
    AddTv {
        record: Box<TvRecord>,
        reply: Option<Reply<manager::AddTv>>,
    },
    RemoveTv {
        id: Uuid,
        reply: Option<Reply<tv::Remove>>,
    },
    /// Request a (re)connect attempt for a TV that is down. Connection
    /// dials are event-driven — a TV that disconnects stays down until
    /// asked (a powered-off TV must not be fought back on).
    Reconnect {
        id: Uuid,
        reply: Option<Reply<tv::Reconnect>>,
    },
    SetName {
        id: Uuid,
        name: String,
        reply: Option<Reply<tv::SetName>>,
    },
    SetManaged {
        id: Uuid,
        managed: bool,
        reply: Option<Reply<tv::SetManaged>>,
    },
    SetSystemProfile {
        id: Uuid,
        profile: Option<Uuid>,
        reply: Option<Reply<tv::SetSystemProfile>>,
    },
    SetPrimaryInput {
        id: Uuid,
        input: Option<String>,
        reply: Option<Reply<tv::SetPrimaryInput>>,
    },
    SetVolume {
        id: Uuid,
        level: u8,
        reply: Option<Reply<tv::SetVolume>>,
    },
    SetMuted {
        id: Uuid,
        muted: bool,
        reply: Option<Reply<tv::SetMuted>>,
    },
    SwitchInput {
        id: Uuid,
        input: String,
        reply: Option<Reply<tv::SwitchInput>>,
    },
    SetBacklight {
        id: Uuid,
        level: u8,
        reply: Option<Reply<tv::SetBacklight>>,
    },
    SetScreen {
        id: Uuid,
        on: bool,
        /// Resolved intent from a profile dispatch: land the TV on its
        /// `primary_input` once the action completes. Manual clients
        /// never set it.
        restore_input: bool,
        reply: Option<Reply<tv::SetScreen>>,
    },
    PowerOn {
        id: Uuid,
        /// Resolved intent from a profile dispatch, as on `SetScreen`.
        restore_input: bool,
        reply: Option<Reply<tv::PowerOn>>,
    },
    PowerOff {
        id: Uuid,
        reply: Option<Reply<tv::PowerOff>>,
    },
    /// Read the TV's config: top-level fields + the vendor table. The
    /// reactor redacts `Secret` fields against the vendor schema before
    /// the reply leaves the process.
    GetConfig {
        id: Uuid,
        reply: Option<Reply<tv::GetConfig>>,
    },
    /// Patch config: known top-level keys apply to the TV's settings,
    /// everything else merges into the vendor table (which forces a
    /// reconnect, since transport parameters may have changed).
    UpdateConfig {
        id: Uuid,
        patch: tisvlay_protocol::rpc::UpdateConfig,
        reply: Option<Reply<tv::UpdateConfig>>,
    },
    /// Create (or boot-replay) a system profile.
    AddProfile {
        record: Box<ProfileRecord>,
        reply: Option<Reply<profile::Create>>,
    },
    UpdateProfile {
        id: Uuid,
        body: Box<tisvlay_core::SystemProfileConfig>,
        reply: Option<Reply<profile::Update>>,
    },
    DuplicateProfile {
        id: Uuid,
        new_id: Uuid,
        name: String,
        reply: Option<Reply<profile::Duplicate>>,
    },
    /// Refused while any TV references the profile.
    DeleteProfile {
        id: Uuid,
        reply: Option<Reply<profile::Delete>>,
    },
    /// The stock event → action mapping, as system-profile field values —
    /// what an unset profile field inherits.
    ProfileDefaults {
        reply: Option<Reply<profile::Defaults>>,
    },
    /// One TV's resolved mapping: its assigned profile's fields where set,
    /// the stock values everywhere else.
    EffectiveProfile {
        id: Uuid,
        reply: Option<Reply<profile::Effective>>,
    },
    /// Apply a hand edit of the store file. The store writer reads the
    /// file and sends the edit, or the refusal (no file, unreadable,
    /// unparsable) for the world to answer.
    ReloadStore {
        edit: Result<Box<StoreEdit>, WireError>,
        reply: Option<Reply<manager::Reload>>,
    },
}

/// A hand edit of the store file: the snapshot the store writer last
/// wrote (or loaded at boot), and the file as it reads now. A reload
/// applies exactly the difference between the two, so world changes still
/// waiting on a write survive it.
#[derive(Debug)]
pub struct StoreEdit {
    pub base: StoreSnapshot,
    pub disk: StoreSnapshot,
}

/// What the world sends the store writer.
#[derive(Debug)]
pub enum StoreMsg {
    /// The world's state changed: write it.
    Persist(Box<StoreSnapshot>),
    /// A reload finished. `Some` carries the file it applied, which is
    /// now the writer's record of what's on disk; `None` means the world
    /// refused it, so that record stands.
    Reloaded(Option<Box<StoreSnapshot>>),
}

/// A dial failure with its typed reason preserved across the task
/// boundary. Root parity: `failure_reason` composed the pairing
/// `failed` phase from the connect error's typed variant; carrying
/// only a display string here is what lost `PermissionDenied { path }`
/// on the way to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialFailure {
    /// The reason when the error carries one (a device-access failure);
    /// `None` is a generic connect failure.
    pub code: Option<tisvlay_protocol::payload::FailureCode>,
    /// Reason-specific parameter — the device-node path for the
    /// permission case.
    pub path: Option<String>,
    /// Human-readable English message (the log/fallback text).
    pub message: String,
    /// The local device couldn't be opened or set up (permission, missing,
    /// busy, claim refused), as opposed to the TV not answering. A
    /// local-device TV keeps retrying these past its budget; world-side
    /// only, never on the wire.
    pub device_level: bool,
}

impl DialFailure {
    /// Project a core error, preserving the actionable typed cases.
    pub fn from_error(error: &tisvlay_core::Error) -> Self {
        match error {
            tisvlay_core::Error::PermissionDenied { path } => Self {
                code: Some(tisvlay_core::errors::DeviceErrorCode::PermissionDenied.into()),
                path: Some(path.clone()),
                message: error.to_string(),
                device_level: true,
            },
            _ => Self {
                code: None,
                path: None,
                message: error.to_string(),
                device_level: matches!(error, tisvlay_core::Error::DeviceUnavailable { .. }),
            },
        }
    }

    /// An untyped failure (infrastructure errors with no core variant).
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            code: None,
            path: None,
            message: message.into(),
            device_level: false,
        }
    }
}

/// Transport characteristics derived from the vendor config — the
/// connector knows, the world reacts (CEC standby model, screen-via-power
/// emulation, carrier-event scoping). Evaluated by the reactor at
/// enrollment and on every dial spawn, never by a dial's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportInfo {
    /// Connection survives standby (CEC): Connected ≠ power, wake happens
    /// in place on the live handle.
    pub reachable_in_standby: bool,
    /// Goes down with the NIC — carrier events apply.
    pub network_dependent: bool,
    /// Survives a source-IP change (NIC failover keeps the connection).
    pub survives_ip_change: bool,
    /// Needs a local device node (CEC's `/dev/cecN`): device availability
    /// gates its dials, and a disconnect redials.
    pub local_device: bool,
    /// No discrete screen-blank: dispatch screen actions as power.
    pub emulate_screen_via_power: bool,
}

/// Facts pushed by a backend connection task into the world.
#[derive(Debug)]
pub enum BackendEvent {
    /// Dial succeeded: the connection task is live and commands can be sent
    /// through `cmd_tx`. Carries the initial state snapshot and the
    /// connection-derived identity. Config-derived statics (wake support,
    /// transport traits) do NOT ride here — the reactor delivers those via
    /// [`InEvent::TransportFacts`] before any dial outcome exists.
    Connected {
        cmd_tx: mpsc::UnboundedSender<BackendCmd>,
        state: Box<TvState>,
        brand: Option<String>,
        hardware: Option<tisvlay_core::HardwareId>,
        capabilities: Vec<String>,
        retry: tisvlay_core::RetryConfig,
        /// The device-reported link MAC — read only on a pairing dial.
        link_mac: Option<tisvlay_core::MacAddress>,
    },
    /// Dial failed before a connection existed.
    ConnectFailed { reason: DialFailure },
    /// A state snapshot pushed by the device (subscription update).
    State(Box<TvState>),
    /// The connection ended (device closed it, or `power_off` on a
    /// network-style transport).
    Disconnected,
    /// An in-place power-on met a closing link and failed after its rpc
    /// had acked. Sent ahead of the task's `Disconnected`, so the world
    /// re-arms it as a wake dial before the link goes.
    PowerOnLost,
    /// A wake-dial phase for the `progress/power` key (`wol_sent`,
    /// `connecting`; the world adds the terminal `connected`/`failed`,
    /// judged from the connection's power snapshot).
    PowerPhase(tisvlay_protocol::payload::PowerPhase),
    /// The bridge is about to send the TV a power-off or a blank, so the
    /// off edge that follows is TisVlay's own. Sent ahead of the command
    /// from the same task as the TV's reports, so it lands before them.
    SendingOff {
        /// A blank (`ScreenOff`) rather than a power-off.
        blank: bool,
    },
    /// The TV's remote powered this PC off ([`tisvlay_core::TvEvent::RemotePower`]).
    RemotePower,
    /// A reply-carrying command finished at the TV — answer the rpc.
    CommandDone {
        reply: AckReply,
        result: Result<(), String>,
    },
}

/// Commands the world sends to a live connection task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCmd {
    /// Settings (and rpc power-off) carry their reply correlation: the
    /// caller's rpc is answered only when the TV acknowledged, so a
    /// well-behaved client paces its writes at device speed instead of
    /// flooding this queue with stale absolute values.
    SetVolume(u8, Option<AckReply>),
    SetMuted(bool, Option<AckReply>),
    SwitchInput(String, Option<AckReply>),
    SetBacklight(u8, Option<AckReply>),
    PowerOff(Option<AckReply>),
    /// Wake and screen dispatch reply through their own machinery (the
    /// power-progress key and the verify-retry tier).
    PowerOn,
    ScreenOn,
    ScreenOff,
    /// Let the TV remote's keys reach the desktop, or stop them.
    SetRemoteKeys(bool),
    /// Dropped once the commands queued before it have run.
    Drain(DrainToken),
}

/// The wake list: every TV's local device node with its system profile's
/// `on_tv_power_on`, for the daemon to write where the wakeup unit reads it.
pub type WakeList = std::collections::BTreeMap<std::path::PathBuf, tisvlay_core::SystemWakeAction>;

/// The machine's sleep a system profile asked for on a going-off event (the
/// TV powering off by itself, its remote powering the PC off), for the
/// daemon's logind adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepRequest {
    /// The TV whose event asked.
    pub tv: Uuid,
    /// How to sleep.
    pub mode: SleepMode,
}

/// How the daemon puts the machine to sleep: logind's modes, without the
/// profile's `none`, so a request always asks for something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepMode {
    /// logind's `Suspend`: suspend to RAM.
    Suspend,
    /// logind's `Sleep`: the host's configured mode (`SleepOperation=`).
    Sleep,
}

impl SleepMode {
    /// The action's name on `event/dispatch-failed`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Sleep => "sleep",
        }
    }
}

/// A token each live connection holds behind the commands already queued
/// on it, dropping it once it has run them. The paired [`Drained`]
/// resolves when every copy is gone: the logind watcher holds the sleep
/// inhibitor until then, so a sleep's TV commands land before the
/// suspend. Only drops carry meaning; nothing is sent on the channel.
#[derive(Debug, Clone)]
pub struct DrainToken(mpsc::Sender<()>);

impl DrainToken {
    /// A fresh token and the [`Drained`] its drops resolve.
    pub fn pair() -> (Self, Drained) {
        let (tx, rx) = mpsc::channel(1);
        (Self(tx), Drained(rx))
    }
}

impl PartialEq for DrainToken {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
}

impl Eq for DrainToken {}

/// Resolves once every copy of its [`DrainToken`] has dropped.
#[derive(Debug)]
pub struct Drained(mpsc::Receiver<()>);

impl Drained {
    /// Wait until every connection has run what was queued before the
    /// token reached it.
    pub async fn wait(mut self) {
        // `recv` yields `None` once every sender is gone.
        let _ = self.0.recv().await;
    }
}

/// One TV's persistable state — the store row, snapshotted from the world
/// whenever config-shaped components change.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TvRecord {
    pub id: Uuid,
    pub name: String,
    pub vendor: String,
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_profile: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_input: Option<String>,
    /// Vendor config verbatim, credentials included — disk only, never
    /// the wire.
    pub config: VendorConfig,
}

/// One system profile's persistable row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileRecord {
    pub id: Uuid,
    pub body: tisvlay_core::SystemProfileConfig,
}

/// The full persisted state: everything the store file holds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StoreSnapshot {
    /// Schema version. Absence *defines* version 1 (the pre-tag
    /// baseline) — a definition, not a compat shim; migration logic
    /// waits until a v2 exists. Exactly one current schema at a time.
    #[serde(default = "schema_v1")]
    pub version: u32,
    #[serde(default)]
    pub tv: Vec<TvRecord>,
    #[serde(default)]
    pub system_profile: Vec<ProfileRecord>,
}

impl Default for StoreSnapshot {
    fn default() -> Self {
        Self {
            version: schema_v1(),
            tv: Vec::new(),
            system_profile: Vec::new(),
        }
    }
}

/// The current persisted-schema version, and the serde default that
/// defines an untagged file as v1.
pub(crate) fn schema_v1() -> u32 {
    1
}

/// Outbound wire-facing effects. The bridge maps these 1:1 onto zenoh
/// operations (put / delete / query reply).
#[derive(Debug, Clone, PartialEq)]
pub enum WireEffect {
    Publish {
        key: String,
        payload: serde_json::Value,
    },
    Delete {
        key: String,
    },
    Reply {
        to: ReplyTo,
        result: Result<serde_json::Value, WireError>,
    },
}

pub use tisvlay_protocol::error::WireError;

#[cfg(test)]
mod tests {
    use super::*;

    /// A pairing update that outlives its session is debug-logged whole; the
    /// config it carries holds the client key, so only key names may show.
    #[test]
    fn pairing_update_debug_shows_only_config_keys() {
        let mut config = VendorConfig::new();
        config.insert("ip", "192.0.2.1");
        config.insert("client_key", "secret-client-key-1234");
        let event = InEvent::PairingUpdate {
            id: Uuid::nil(),
            update: PairingUpdate::ConfigReady(config),
        };
        for shown in [format!("{event:?}"), format!("{event:#?}")] {
            assert!(!shown.contains("secret-client-key-1234"), "{shown}");
            assert!(!shown.contains("192.0.2.1"), "{shown}");
            assert!(
                shown.contains("client_key"),
                "key names stay visible: {shown}"
            );
        }
    }
}
