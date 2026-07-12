//! Inbound events, commands, and outbound effects — the world's entire I/O
//! vocabulary.

use bevy_ecs::message::Message;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tisvlay_core::TvState;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Correlation id for a command reply — the bridge maps it back onto the
/// zenoh query that asked.
pub type ReplyTo = u64;

/// Everything that can enter the world through the reactor inbox.
#[derive(Message, Debug)]
pub enum InEvent {
    /// A control-plane command (RPC via the bridge, or a test driver).
    Command(Command),
    /// A fact reported by a backend connection task.
    Backend { tv: Uuid, event: BackendEvent },
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
    /// The mirrored remote-session fact: the set of TVs the session
    /// service computed as armed (a remote session is active and the
    /// resolved user profile arms it). Consumed into the `RemoteArmed`
    /// resource; an empty set disarms every TV. The world sees only the
    /// armed projection — arming policy stays session-side.
    RemoteSession { armed: Vec<Uuid> },
    /// NIC carrier transition — parks/unparks connections whose
    /// monitored interface (route-resolved NIC) matches `interface`.
    Carrier { interface: String, up: bool },
    /// Route resolution outcome for a TV: the NIC carrying traffic to
    /// its address (`ip route get`), or `None` when no usable route
    /// exists. Sent by the daemon's netlink monitor at monitor-request
    /// time and whenever the kernel routing table changes.
    RouteResolved { tv: Uuid, interface: Option<String> },
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
    Handle(tokio::sync::mpsc::Sender<tisvlay_core::PairingCommand>),
    /// A pairing state transition (progress or terminal).
    State(Box<tisvlay_core::PairingState>),
    /// The reactor built the final vendor config from credentials — spawn
    /// the TV (or update it, for a re-pair).
    ConfigReady(toml::Value),
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
        name: String,
        vendor: String,
        config: toml::Value,
        managed: bool,
        reply: Option<ReplyTo>,
    },
    /// Re-run the pairing handshake for an existing TV (refreshes
    /// credentials); progress streams on the TV's pairing key.
    RePair {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    CancelPair {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    /// Enroll a TV directly from a full store row — the boot-replay and
    /// rpc enrollment path alike.
    AddTv {
        record: Box<TvRecord>,
        reply: Option<ReplyTo>,
    },
    RemoveTv {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    /// Request a (re)connect attempt for a TV that is down. Connection
    /// dials are event-driven — a TV that disconnects stays down until
    /// asked (a powered-off TV must not be fought back on).
    Reconnect {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    SetName {
        id: Uuid,
        name: String,
        reply: Option<ReplyTo>,
    },
    SetManaged {
        id: Uuid,
        managed: bool,
        reply: Option<ReplyTo>,
    },
    SetSystemProfile {
        id: Uuid,
        profile: Option<Uuid>,
        reply: Option<ReplyTo>,
    },
    SetPrimaryInput {
        id: Uuid,
        input: Option<String>,
        reply: Option<ReplyTo>,
    },
    SetVolume {
        id: Uuid,
        level: u8,
        reply: Option<ReplyTo>,
    },
    SetMuted {
        id: Uuid,
        muted: bool,
        reply: Option<ReplyTo>,
    },
    SwitchInput {
        id: Uuid,
        input: String,
        reply: Option<ReplyTo>,
    },
    SetBacklight {
        id: Uuid,
        level: u8,
        reply: Option<ReplyTo>,
    },
    SetScreen {
        id: Uuid,
        on: bool,
        /// Resolved intent from a profile dispatch: land the TV on its
        /// `primary_input` once the action completes. Manual clients
        /// never set it.
        restore_input: bool,
        reply: Option<ReplyTo>,
    },
    PowerOn {
        id: Uuid,
        /// Resolved intent from a profile dispatch, as on `SetScreen`.
        restore_input: bool,
        reply: Option<ReplyTo>,
    },
    PowerOff {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    /// Read the TV's config: top-level fields + the vendor table. The
    /// reactor redacts `Secret` fields against the vendor schema before
    /// the reply leaves the process.
    GetConfig {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    /// Patch config: known top-level keys apply to the TV's settings,
    /// everything else merges into the vendor table (which forces a
    /// reconnect, since transport parameters may have changed).
    UpdateConfig {
        id: Uuid,
        patch: serde_json::Value,
        reply: Option<ReplyTo>,
    },
    /// Create (or boot-replay) a system profile.
    AddProfile {
        record: Box<ProfileRecord>,
        reply: Option<ReplyTo>,
    },
    UpdateProfile {
        id: Uuid,
        body: Box<tisvlay_core::SystemProfileConfig>,
        reply: Option<ReplyTo>,
    },
    DuplicateProfile {
        id: Uuid,
        new_id: Uuid,
        name: String,
        reply: Option<ReplyTo>,
    },
    /// Refused while any TV references the profile.
    DeleteProfile {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    /// The stock event → action mapping, as system-profile field values —
    /// what an unset profile field inherits.
    ProfileDefaults {
        reply: Option<ReplyTo>,
    },
    /// One TV's resolved mapping: its assigned profile's fields where set,
    /// the stock values everywhere else.
    EffectiveProfile {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
}

/// A dial failure with its typed reason preserved across the task
/// boundary. Root parity: `failure_reason` composed the pairing
/// `failed` phase from the connect error's typed variant; carrying
/// only a display string here is what lost `PermissionDenied { path }`
/// on the way to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialFailure {
    /// Namespaced reason code when the error has one — today
    /// `device.permission-denied`; `None` = generic connect failure.
    pub code: Option<String>,
    /// Reason-specific parameter — the device-node path for the
    /// permission case.
    pub path: Option<String>,
    /// Human-readable English message (the log/fallback text).
    pub message: String,
}

impl DialFailure {
    /// Project a core error, preserving the actionable typed cases.
    pub fn from_error(error: &tisvlay_core::Error) -> Self {
        match error {
            tisvlay_core::Error::PermissionDenied { path } => Self {
                code: Some(
                    tisvlay_core::errors::DeviceErrorCode::PermissionDenied
                        .as_str()
                        .into(),
                ),
                path: Some(path.clone()),
                message: error.to_string(),
            },
            _ => Self {
                code: None,
                path: None,
                message: error.to_string(),
            },
        }
    }

    /// An untyped failure (infrastructure errors with no core variant).
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            code: None,
            path: None,
            message: message.into(),
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
    },
    /// Dial failed before a connection existed.
    ConnectFailed { reason: DialFailure },
    /// A state snapshot pushed by the device (subscription update).
    State(Box<TvState>),
    /// The connection ended (device closed it, or `power_off` on a
    /// network-style transport).
    Disconnected,
    /// A wake-dial phase for the `progress/power` key (`wol_sent`,
    /// `connecting`; the world adds the terminal `connected`/`failed`).
    PowerPhase(&'static str),
    /// A reply-carrying command finished at the TV — answer the rpc.
    CommandDone {
        reply: ReplyTo,
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
    SetVolume(u8, Option<ReplyTo>),
    SetMuted(bool, Option<ReplyTo>),
    SwitchInput(String, Option<ReplyTo>),
    SetBacklight(u8, Option<ReplyTo>),
    PowerOff(Option<ReplyTo>),
    /// Wake and screen dispatch reply through their own machinery (the
    /// power-progress key and the verify-retry tier).
    PowerOn,
    ScreenOn,
    ScreenOff,
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
    pub config: toml::Value,
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

pub use tisvlay_wire::error::WireError;
