//! Wire payload structs: each is the serde wire payload and the value at
//! one key ([`StatePayload::PATH`] under the TV's key prefix). The daemon's
//! world stores them as components, wrapped on its side, so this crate
//! knows nothing of its ECS.

use serde::{Deserialize, Serialize};

/// A payload that lives at one per-TV key. `PATH` is the key tail under
/// `tisvlay/<site>/tv/<uuid>/`.
pub trait StatePayload: Serialize + Clone + PartialEq + Send + Sync + 'static {
    /// The key tail, e.g. `state/power`.
    const PATH: &'static str;
}

/// Power state, projected from the vendor-reported substate.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Power {
    /// The vendor-reported substate, which keeps distinctions (active
    /// standby vs screen off) the two booleans collapse.
    pub state: tisvlay_core::PowerState,
    /// Whether the TV is powered on.
    pub is_on: bool,
    /// Whether the panel shows a picture; `false` while blanked.
    pub is_screen_on: bool,
}

impl StatePayload for Power {
    const PATH: &'static str = "state/power";
}

impl From<&tisvlay_core::TvState> for Power {
    fn from(s: &tisvlay_core::TvState) -> Self {
        Self {
            state: s.power_state,
            is_on: s.is_on,
            is_screen_on: s.is_screen_on,
        }
    }
}

/// Audio state. Published once the backend reports a volume, so a TV
/// whose backend reports none has no value at this key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Volume, 0–100.
    pub level: u8,
    /// Whether audio is muted; `false` when the backend doesn't
    /// report mute.
    pub muted: bool,
}

impl StatePayload for Volume {
    const PATH: &'static str = "state/volume";
}

/// Connection health — derived from world structure (a TV with a live
/// connection task is `connected`; one mid-dial is `reconnecting`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// A live connection serves commands.
    pub connected: bool,
    /// A dial is in flight right now.
    pub reconnecting: bool,
    /// Whether the TV can be woken from standby with its current
    /// config (for LG, a configured MAC for Wake-on-LAN). Known before
    /// the first dial.
    pub supports_wake: bool,
    /// What a disconnected TV is waiting for, when it's known gone: its
    /// network route, or the local device its connection runs through
    /// (a USB CEC adapter). `None` while nothing is known missing.
    pub waiting_for: Option<LinkWait>,
}

/// What a TV's connection is waiting for, on [`Link::waiting_for`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LinkWait {
    /// No usable route to the TV's address.
    Network,
    /// The local device the connection needs isn't there or isn't usable.
    Device,
}

impl LinkWait {
    /// The wire token, for display and the GUI's string properties.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Device => "device",
        }
    }
}

impl StatePayload for Link {
    const PATH: &'static str = "state/link";
}

/// Current + available inputs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// The active input's id, in the same id space as `available`;
    /// `None` when the TV doesn't report it.
    pub current: Option<String>,
    /// The TV's inputs; empty when the backend doesn't list them.
    pub available: Vec<InputEntry>,
}

/// One of a TV's inputs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InputEntry {
    /// Backend input id (LG: `HDMI_1`), as `switch-input` and
    /// `set-primary-input` take it.
    pub id: String,
    /// Display label (`HDMI 1`, `PC`).
    pub label: String,
    /// Whether a device is plugged into the input.
    pub connected: bool,
}

impl StatePayload for Input {
    const PATH: &'static str = "state/input";
}

/// Backlight (OLED pixel brightness). Published once the backend
/// reports one, so a TV whose backend reports none has no value at
/// this key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Backlight {
    /// Backlight, 0–100.
    pub level: u8,
}

impl StatePayload for Backlight {
    const PATH: &'static str = "state/backlight";
}

/// Who the TV is: its name, backend, and what the device reported
/// about itself on connect.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Display name. A blank one takes the device-reported `model` on
    /// connect.
    pub name: String,
    /// Backend vendor id from the `manager/vendors` catalog.
    pub vendor: String,
    /// Canonical device brand when the backend can resolve it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    /// EDID-style three-letter manufacturer code (`GSM` for LG), as
    /// the device reports it on connect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    /// Model name or product code (`OLED65G4SUB`), as the device
    /// reports it on connect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Unit serial number, as the device reports it on connect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// The connected backend's lowercase capability names (`power`,
    /// `volume`, ...); empty until the first connect.
    pub capabilities: Vec<String>,
    /// DRM connector this TV mirrors, once the desktop probe matches it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// The vendor's primary identifying descriptor (LG: IP; CEC:
    /// device node) — the connector's `primary_descriptor`, evaluated
    /// by the reactor from the vendor config. Clients render it as
    /// the TV's spatial hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

impl Identity {
    /// A fresh identity carrying only what enrollment knows.
    pub fn new(name: impl Into<String>, vendor: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            vendor: vendor.into(),
            brand: None,
            make: None,
            model: None,
            serial: None,
            capabilities: Vec::new(),
            connector: None,
            primary: None,
        }
    }
}

impl StatePayload for Identity {
    const PATH: &'static str = "meta/identity";
}

/// Vendor-projected display rows for the TV's read-only INFO surface:
/// the connector's `display_config` (editable fields) and
/// `display_diagnostics` (pair-time captures) as schema-ordered,
/// labeled `(label, value)` pairs, Secret-free. Arrays preserve order
/// on the wire — JSON objects re-sort alphabetically — so this is the
/// only shape row order survives in.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct VendorRows {
    /// Rows for the vendor's editable config fields.
    pub config: Vec<(String, String)>,
    /// Rows for what the vendor captured at pairing time.
    pub diagnostics: Vec<(String, String)>,
}

impl StatePayload for VendorRows {
    const PATH: &'static str = "meta/vendor";
}

/// The TV's settings and the automation state derived from them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Whether the TV's profiles act, on the TV and on this PC for the
    /// TV's own events. An unmanaged TV stays connected for manual
    /// control.
    pub managed: bool,
    /// The assigned system profile; absent follows the default
    /// profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_profile: Option<uuid::Uuid>,
    /// The input restore intents land on and `automate_when` compares
    /// against, an id from `state/input`; absent when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_input: Option<String>,
    /// Read-only projection of the resolved profile-chain
    /// `automate_when` policy (assigned profile - default profile -
    /// always); the world's derive pass is the only writer.
    pub automate_when: tisvlay_core::AutomateWhen,
    /// The evaluated automation gate: `managed` AND the resolved
    /// `automate_when` holds against the current input. Consumers
    /// (session service, tray) act only on automated TVs.
    pub automated: bool,
    /// Whether a stream currently arms this TV (mirrored
    /// from the session service). While set, the world honors the
    /// system profile's while-streaming overrides on DPMS-on/resume; a
    /// read-only projection for clients to surface the state.
    #[serde(default)]
    pub stream_armed: bool,
}

impl Meta {
    /// A fresh TV's settings: no profile or primary input assigned,
    /// the `always` policy, and automated exactly when managed.
    pub fn new(managed: bool) -> Self {
        Self {
            managed,
            system_profile: None,
            primary_input: None,
            automate_when: tisvlay_core::AutomateWhen::Always,
            // `should_automate` gives unknown inputs the benefit of
            // the doubt, so a fresh managed TV starts automated.
            automated: managed,
            // No stream until the mirror says so.
            stream_armed: false,
        }
    }
}

impl StatePayload for Meta {
    const PATH: &'static str = "meta/config";
}

/// Every per-TV cached-state key tail — the DELETE sweep on TV removal.
pub const TV_PATHS: [&str; 8] = [
    Power::PATH,
    Volume::PATH,
    Link::PATH,
    Input::PATH,
    Backlight::PATH,
    Identity::PATH,
    Meta::PATH,
    VendorRows::PATH,
];

/// Non-payload per-TV keys included in the removal sweep.
pub const TV_EXTRA_PATHS: [&str; 1] = ["progress/power"];

/// Pairing progress on `tisvlay/<site>/pairing/<uuid>/progress` — the
/// redacted projection of the core pairing state: credentials never
/// reach the wire, clients branch on `phase` and localize by `code`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PairingProgress {
    /// The pairing attempt this sample belongs to, minted per pair /
    /// re-pair and returned by that rpc. A TV's progress key outlives
    /// its sessions — a history subscriber sees earlier attempts'
    /// samples replayed from the cache, in an order relative to the
    /// live ones that isn't guaranteed — so a watcher drains only its
    /// own attempt.
    pub attempt: uuid::Uuid,
    /// Where the attempt stands.
    pub phase: PairingPhase,
    /// Human-readable status in English: the vendor's prompt while
    /// awaiting, the error text on failure. Clients localize from
    /// `phase` and `code` instead.
    pub message: String,
    /// Why the attempt failed; `failed` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<FailureCode>,
    /// Reason-specific parameter — the device-node path on a
    /// permission-denied failure, so clients can name the node in
    /// the fix hint. Absent for reasons that carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `complete` only: what the device reported as its link MAC and
    /// what the daemon did with it. Absent when the vendor can't
    /// report one or the read failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_mac: Option<LinkMacReport>,
}

/// The device-reported link MAC at pairing time, compared against the
/// TV's configured `mac` by the daemon so clients only render it. A
/// snapshot of the moment the pairing completed — accepting the
/// reported MAC afterwards doesn't rewrite this sample.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LinkMacReport {
    /// The device-reported MAC, canonical form.
    pub reported: String,
    /// How it compared to the configured `mac`.
    pub outcome: LinkMacOutcome,
}

/// What the daemon did with a device-reported link MAC, tagged by
/// `kind` on the wire. The daemon never overwrites a configured MAC.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkMacOutcome {
    /// `mac` was unset; the daemon stored `reported`.
    Filled,
    /// The configured `mac` already equals `reported`.
    Matches,
    /// The configured `mac` differs and was kept.
    Differs {
        /// The configured value as written, not canonicalized.
        configured: String,
    },
}

/// A wake-and-connect flow's progress, on a TV's `progress/power` key.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerProgress {
    /// The step the flow just reached.
    pub phase: PowerPhase,
}

/// The TV powered off by itself or from its own remote, at
/// [`keys::tv_power_off`](crate::keys::tv_power_off): a live edge from on
/// to off between two reports on one connection, which TisVlay's own
/// power-offs and blanks don't produce. A connection's first reading or a
/// dropped link never sends it. Live only: subscribe without history.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TvPowerOff {
    /// The TV that powered off.
    pub tv: uuid::Uuid,
}

/// The TV's remote powered this PC off, at
/// [`keys::remote_power`](crate::keys::remote_power): on HDMI-CEC, the TV's
/// "power off this device" aimed at the PC, once per press. Live only:
/// subscribe without history.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemotePower {
    /// The TV whose remote it was.
    pub tv: uuid::Uuid,
}

/// The phases a wake-and-connect flow walks through, ending in
/// `connected` or `failed`. The world judges the end from the power state
/// the wake left: a wake fails only if the TV reports an off state
/// (`PowerState::class` is `Off`), so a TV that's warming up, cooling
/// down or can't report its state ends `connected`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PowerPhase {
    /// The Wake-on-LAN packet has been (or is about to be) sent.
    WolSent,
    /// The wake dial is in flight.
    Connecting,
    /// The dial is deferred until the TV's network route returns.
    AwaitingNetwork,
    /// The dial is deferred until the local device the connection needs
    /// (a USB CEC adapter) returns.
    AwaitingDevice,
    /// The wake dial connected and the TV didn't report an off state: it
    /// acted on the wake.
    Connected,
    /// The wake didn't land: the dial ran out of attempts, or it connected
    /// to a TV that still reported an off state, whose link then stays up.
    /// It's also the only phase a power-on over a live link publishes,
    /// when the TV stayed off; one an off-going command interrupted
    /// publishes nothing.
    Failed,
}

impl PowerPhase {
    /// The wire token, for display and the GUI's string properties.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WolSent => "wol_sent",
            Self::Connecting => "connecting",
            Self::AwaitingNetwork => "awaiting_network",
            Self::AwaitingDevice => "awaiting_device",
            Self::Connected => "connected",
            Self::Failed => "failed",
        }
    }
}

/// Where a pairing attempt stands.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PairingPhase {
    /// Waiting for the user to accept on the TV.
    AwaitingTv,
    /// Waiting for the user to enter a PIN or password.
    AwaitingInput,
    /// Paired, and the TV's first dial succeeded.
    Complete,
    /// The attempt ended without pairing; `code` says why.
    Failed,
}

impl PairingPhase {
    /// `complete` or `failed`: no further sample follows for the
    /// attempt.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed)
    }

    /// The wire token (`awaiting_tv`, ...), for display and the GUI's
    /// string properties.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingTv => "awaiting_tv",
            Self::AwaitingInput => "awaiting_input",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

/// Why a pairing attempt failed: the handshake's `pairing.*` reasons
/// and the first dial's `device.*` access reasons, on the wire as
/// those namespaced tokens.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FailureCode {
    /// The user rejected pairing on the TV.
    #[serde(rename = "pairing.rejected")]
    PairingRejected,
    /// No response from the TV or the user in time.
    #[serde(rename = "pairing.timeout")]
    PairingTimeout,
    /// The TV couldn't be reached.
    #[serde(rename = "pairing.connection-failed")]
    PairingConnectionFailed,
    /// The PIN or password was wrong.
    #[serde(rename = "pairing.invalid-input")]
    PairingInvalidInput,
    /// The attempt was cancelled (`manager/rpc/cancel-pair`).
    #[serde(rename = "pairing.cancelled")]
    PairingCancelled,
    /// A handshake failure this wire has no code for, or an internal
    /// error.
    #[serde(rename = "pairing.unknown")]
    PairingUnknown,
    /// The daemon may not open the device node (`path` names it).
    #[serde(rename = "device.permission-denied")]
    DevicePermissionDenied,
    /// A device failure this wire has no code for yet.
    #[serde(rename = "device.unknown")]
    DeviceUnknown,
}

impl FailureCode {
    /// The wire token, for display.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PairingRejected => "pairing.rejected",
            Self::PairingTimeout => "pairing.timeout",
            Self::PairingConnectionFailed => "pairing.connection-failed",
            Self::PairingInvalidInput => "pairing.invalid-input",
            Self::PairingCancelled => "pairing.cancelled",
            Self::PairingUnknown => "pairing.unknown",
            Self::DevicePermissionDenied => "device.permission-denied",
            Self::DeviceUnknown => "device.unknown",
        }
    }
}

impl std::fmt::Display for FailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<tisvlay_core::PairingErrorCode> for FailureCode {
    fn from(code: tisvlay_core::PairingErrorCode) -> Self {
        use tisvlay_core::PairingErrorCode;
        match code {
            PairingErrorCode::Rejected => Self::PairingRejected,
            PairingErrorCode::Timeout => Self::PairingTimeout,
            PairingErrorCode::ConnectionFailed => Self::PairingConnectionFailed,
            PairingErrorCode::InvalidInput => Self::PairingInvalidInput,
            PairingErrorCode::Cancelled => Self::PairingCancelled,
            // Core's codes are non-exhaustive: one this wire doesn't
            // know yet reads as unknown until it gets a variant here.
            PairingErrorCode::Unknown | _ => Self::PairingUnknown,
        }
    }
}

impl From<tisvlay_core::errors::DeviceErrorCode> for FailureCode {
    fn from(code: tisvlay_core::errors::DeviceErrorCode) -> Self {
        match code {
            tisvlay_core::errors::DeviceErrorCode::PermissionDenied => Self::DevicePermissionDenied,
            // As above, in the device family: a code this wire
            // doesn't know yet.
            _ => Self::DeviceUnknown,
        }
    }
}

impl std::fmt::Display for PairingPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&tisvlay_core::PairingState> for PairingPhase {
    fn from(state: &tisvlay_core::PairingState) -> Self {
        use tisvlay_core::PairingState;
        match state {
            PairingState::AwaitingTvPrompt { .. } => Self::AwaitingTv,
            PairingState::AwaitingUserInput { .. } => Self::AwaitingInput,
            PairingState::Complete { .. } => Self::Complete,
            PairingState::Failed { .. } => Self::Failed,
        }
    }
}

impl PairingProgress {
    /// Project a core pairing state onto `attempt`'s progress sample.
    pub fn from_state(attempt: uuid::Uuid, state: &tisvlay_core::PairingState) -> Self {
        Self {
            attempt,
            phase: state.into(),
            message: state.message().into(),
            code: state.error().map(|e| e.code.into()),
            // Handshake errors carry no device parameter; dial
            // failures (which do) are composed by the world from
            // the typed dial reason, not through this projection.
            path: None,
            link_mac: None,
        }
    }
}

/// A stream fact: every live source tagged by kind, plus the
/// per-TV `armed` projection the session computes from the sources
/// crossed with each TV's resolved user profile. The world (via the
/// system mirror) consumes `armed` booleans only, staying
/// arming-policy-free.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamState {
    /// Every live source; empty when no stream is detected.
    pub sources: Vec<StreamSource>,
    /// The TVs a live source currently arms, sorted by id.
    pub armed: Vec<uuid::Uuid>,
}

/// One live stream source. `detail` names the specific origin (the
/// resolved exe for `Portal`, the source label for `Manual`); it's
/// carried for display and attribution, not routing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StreamSource {
    /// How the source was detected.
    pub kind: StreamSourceKind,
    /// The specific origin; a source is unique per `(kind, detail)`.
    /// Each [`StreamSourceKind`] says what it puts here.
    pub detail: String,
}

/// How a stream source was detected. Attributed kinds always arm;
/// the unattributed `Screencast` signal arms only when the resolved
/// profile opts in (`screencast_arms`). `Ord` so the source
/// set keys deterministically.
///
/// `Sunshine` is the continuous `/serverinfo` level poll's own key,
/// kept distinct from the `Manual` hook verb so the poll and the
/// optional `global_prep_cmd` hook coexist without racing each other's
/// arm.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum StreamSourceKind {
    /// Toggled through `user/stream/rpc/set`; `detail` is the
    /// caller's source label.
    Manual,
    /// A remote graphical logind session of this user; `detail` is
    /// the remote host, or `session <id>` when it names none.
    Logind,
    /// A desktop-portal screencast or remote-desktop session;
    /// `detail` is the calling app's resolved executable.
    Portal,
    /// A screen capture seen in the PipeWire graph, with no app
    /// attribution; `detail` summarizes the capture nodes.
    Screencast,
    /// Sunshine's `/serverinfo` reports a stream in progress;
    /// `detail` is `serverinfo`.
    Sunshine,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_core::{Credentials, InputType, PairingError, PairingState};

    /// Each code's wire token matches core's `as_str()`, so clients
    /// print the reason names core defines.
    #[test]
    fn failure_codes_keep_core_tokens() {
        use tisvlay_core::PairingErrorCode as P;
        for code in [
            P::Rejected,
            P::Timeout,
            P::ConnectionFailed,
            P::InvalidInput,
            P::Cancelled,
            P::Unknown,
        ] {
            assert_eq!(FailureCode::from(code).as_str(), code.as_str());
        }
        let device = tisvlay_core::errors::DeviceErrorCode::PermissionDenied;
        assert_eq!(FailureCode::from(device).as_str(), device.as_str());
        for code in [
            FailureCode::PairingRejected,
            FailureCode::DevicePermissionDenied,
            FailureCode::DeviceUnknown,
        ] {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, code.as_str());
            assert_eq!(serde_json::from_value::<FailureCode>(json).unwrap(), code);
        }
    }

    /// Every phase the world publishes decodes, the waiting ones
    /// included.
    #[test]
    fn every_power_phase_decodes() {
        for phase in [
            PowerPhase::WolSent,
            PowerPhase::Connecting,
            PowerPhase::AwaitingNetwork,
            PowerPhase::AwaitingDevice,
            PowerPhase::Connected,
            PowerPhase::Failed,
        ] {
            let sample = serde_json::json!({ "phase": phase.as_str() });
            let progress: PowerProgress = serde_json::from_value(sample).unwrap();
            assert_eq!(progress.phase, phase);
        }
    }

    #[test]
    fn link_waits_round_trip_their_tokens() {
        for wait in [LinkWait::Network, LinkWait::Device] {
            let json = serde_json::to_value(wait).unwrap();
            assert_eq!(json, wait.as_str());
            assert_eq!(serde_json::from_value::<LinkWait>(json).unwrap(), wait);
        }
        let link = Link {
            connected: false,
            reconnecting: false,
            supports_wake: true,
            waiting_for: Some(LinkWait::Device),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["waiting_for"], "device");
        assert_eq!(serde_json::from_value::<Link>(json).unwrap(), link);
    }

    #[test]
    fn pairing_states_map_to_their_phases() {
        let cases = [
            (
                PairingState::awaiting_tv_prompt("accept"),
                PairingPhase::AwaitingTv,
            ),
            (
                PairingState::awaiting_user_input("pin", InputType::Pin),
                PairingPhase::AwaitingInput,
            ),
            (
                PairingState::complete(Credentials::new("key")),
                PairingPhase::Complete,
            ),
            (
                PairingState::failed(PairingError::rejected()),
                PairingPhase::Failed,
            ),
        ];
        for (state, phase) in cases {
            assert_eq!(PairingPhase::from(&state), phase);
            assert_eq!(
                phase.is_terminal(),
                state.is_complete() || state.is_failed()
            );
        }
    }
}
