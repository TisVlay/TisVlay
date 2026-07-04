//! Inbound events, commands, and outbound effects — the world's entire I/O
//! vocabulary.

use bevy_ecs::message::Message;
use std::time::Instant;
use tisvlay_core::TvState;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Correlation id for a command reply. In M2 this becomes the zenoh query
/// handle; for now it is an opaque number the test (or RPC glue) chooses.
pub type ReplyTo = u64;

/// Everything that can enter the world through the reactor inbox.
#[derive(Message, Debug)]
pub enum InEvent {
    /// A control-plane command (RPC in M2, test driver today).
    Command(Command),
    /// A fact reported by a backend connection task.
    Backend { tv: Uuid, event: BackendEvent },
    /// Time advance. Consumed by the reactor itself (updates the world
    /// clock); never dispatched as a bevy message.
    Tick { now: Instant },
}

#[derive(Debug)]
pub enum Command {
    AddTv {
        id: Uuid,
        name: String,
        vendor: String,
        config: toml::Value,
        managed: bool,
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
    PowerOn {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
    PowerOff {
        id: Uuid,
        reply: Option<ReplyTo>,
    },
}

/// Facts pushed by a backend connection task into the world.
#[derive(Debug)]
pub enum BackendEvent {
    /// Dial succeeded: the connection task is live and commands can be sent
    /// through `cmd_tx`. Carries the initial state snapshot.
    Connected {
        cmd_tx: mpsc::UnboundedSender<BackendCmd>,
        state: Box<TvState>,
        supports_wake: bool,
    },
    /// Dial failed before a connection existed.
    ConnectFailed { error: String },
    /// A state snapshot pushed by the device (subscription update).
    State(Box<TvState>),
    /// The connection ended (device closed it, or `power_off` on a
    /// network-style transport).
    Disconnected,
}

/// Commands the world sends to a live connection task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCmd {
    SetVolume(u8),
    SetMuted(bool),
    PowerOn,
    PowerOff,
}

/// Outbound wire-facing effects. In M2 these map 1:1 onto zenoh operations
/// (put / delete / query reply); today the effects channel is the observable
/// contract the tests assert on.
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

/// Typed failure reason for a command reply — serialized into the zenoh
/// `reply_err` payload in M2.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

impl WireError {
    pub fn tv_not_found(id: Uuid) -> Self {
        Self {
            code: "tv-not-found".into(),
            message: format!("no TV with id {id}"),
        }
    }

    pub fn tv_exists(id: Uuid) -> Self {
        Self {
            code: "tv-exists".into(),
            message: format!("a TV with id {id} already exists"),
        }
    }

    pub fn not_connected(id: Uuid) -> Self {
        Self {
            code: "not-connected".into(),
            message: format!("TV {id} has no live connection"),
        }
    }

    pub fn unknown_vendor(vendor: &str) -> Self {
        Self {
            code: "unknown-vendor".into(),
            message: format!("no backend registered for vendor '{vendor}'"),
        }
    }
}
