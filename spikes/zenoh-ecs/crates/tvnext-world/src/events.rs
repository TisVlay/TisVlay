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
    /// NIC carrier transition — parks/unparks network-dependent
    /// connections.
    Carrier { up: bool },
    /// The desktop probe matched a TV's hardware identity to a DRM
    /// connector.
    ConnectorMatched { tv: Uuid, connector: String },
    /// Time advance. Consumed by the reactor itself (updates the world
    /// clock); never dispatched as a bevy message.
    Tick { now: Instant },
}

/// Desktop/power events the profile dispatcher maps to TV actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemEvent {
    Sleep,
    Resume,
    Shutdown,
    DpmsOff,
    DpmsOn,
    /// Login greeter went idle (published by `tvnext-greeter`).
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
    SetAutomateWhen {
        id: Uuid,
        when: String,
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
        reply: Option<ReplyTo>,
    },
    PowerOn {
        id: Uuid,
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

/// Transport characteristics resolved at dial time — the connector knows,
/// the world reacts (CEC standby model, screen-via-power emulation,
/// carrier-event scoping).
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
    /// connection-derived identity.
    Connected {
        cmd_tx: mpsc::UnboundedSender<BackendCmd>,
        state: Box<TvState>,
        supports_wake: bool,
        brand: Option<String>,
        hardware: Option<tisvlay_core::HardwareId>,
        capabilities: Vec<String>,
        traits: TransportInfo,
        retry: tisvlay_core::RetryConfig,
    },
    /// Dial failed before a connection existed.
    ConnectFailed { error: String },
    /// A state snapshot pushed by the device (subscription update).
    State(Box<TvState>),
    /// The connection ended (device closed it, or `power_off` on a
    /// network-style transport).
    Disconnected,
    /// A wake-dial phase for the `progress/power` key (`wol_sent`,
    /// `connecting`; the world adds the terminal `connected`/`failed`).
    PowerPhase(&'static str),
}

/// Commands the world sends to a live connection task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCmd {
    SetVolume(u8),
    SetMuted(bool),
    PowerOn,
    PowerOff,
    ScreenOn,
    ScreenOff,
    SwitchInput(String),
    SetBacklight(u8),
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
    #[serde(default = "default_automate_when")]
    pub automate_when: String,
    /// Vendor config verbatim, credentials included — disk only, never
    /// the wire.
    pub config: toml::Value,
}

fn default_automate_when() -> String {
    "always".into()
}

/// One system profile's persistable row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProfileRecord {
    pub id: Uuid,
    pub body: tisvlay_core::SystemProfileConfig,
}

/// The full persisted state: everything the store file holds.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct StoreSnapshot {
    #[serde(default)]
    pub tv: Vec<TvRecord>,
    #[serde(default)]
    pub system_profile: Vec<ProfileRecord>,
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

pub use tvnext_wire::error::WireError;
