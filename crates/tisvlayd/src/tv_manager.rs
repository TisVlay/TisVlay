//! TV connection manager.
//!
//! Manages connections to multiple TVs using vendor-agnostic trait objects.
//!
//! Uses per-TV locking so that operations on different TVs can proceed
//! concurrently. The outer `RwLock<HashMap>` is held only briefly for
//! lookup; network I/O runs under a per-TV `tokio::sync::Mutex`.

use crate::dbus::{register_tv_interfaces, tv_path, unregister_tv_interfaces};
use crate::registry::BackendRegistry;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long the direct-connect probe in [`TvManager::power_on`]
/// waits before giving up and falling through to Wake-on-LAN.
///
/// Healthy single-attempt reconnects complete in <500 ms when the
/// TV is reachable; this cap is belt-and-suspenders for misbehaving
/// networks (firewall blackholes, hung TLS handshakes) where a
/// connect would otherwise stall on the kernel's multi-second
/// handshake budget. Backends that violate the
/// `TvConnector::connect()` fast-fail contract are also bounded
/// here.
const DIRECT_CONNECT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often [`state_poll_loop`] refreshes state for connected
/// poll-only TVs (backends without a `subscribe_state` push channel).
///
/// Push backends (LG, the default mock) get real-time state and are
/// skipped by the loop entirely, so this cadence only governs how
/// stale a poll-only projector's D-Bus state can be between its own
/// state changes. 20 s keeps the projector's `warming_up` /
/// `cooling_down` / volume / input reasonably fresh without a busy
/// poll; the reliable-dispatch verify loop refreshes power state on
/// demand for the cases that need sub-second accuracy.
const STATE_POLL_INTERVAL: Duration = Duration::from_secs(20);

/// Per-TV cap on a single `get_state` query inside
/// [`state_poll_loop`]. Bounds how long the poll holds a TV's
/// per-`Mutex<ManagedTv>` so a slow or reconnecting projector can't
/// block concurrent `Tv1.*` calls behind the poll for more than this.
/// On timeout the tick skips the TV; the next tick retries.
const STATE_POLL_QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Upper bound on how long [`TvManager::screen_on`] waits for a
/// projector reporting [`PowerState::WarmingUp`] to reach a usable
/// (`Active` / `ScreenSaver`) state before falling through to the
/// normal verify-retry loop.
///
/// Projector laser/lamp warm-up is typically 20-40 s; 60 s is a
/// generous ceiling. Instant-power backends (LG) never report
/// `WarmingUp`, so this never engages for them.
const WARMUP_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Delay between authoritative power-state polls while
/// [`TvManager::screen_on`] waits out a warming-up projector. Each
/// poll issues a live `get_power_state`, so this paces the queries
/// rather than busy-looping.
const WARMUP_POLL_INTERVAL: Duration = Duration::from_secs(2);
use tisvlay_core::{
    ActionExecutor, Capabilities, DEFAULT_PROFILE_NAME, DEFAULT_SYSTEM_PROFILE_UUID,
    FieldVisibility, PowerState, ResolvedSystemProfile, SessionAction, SystemConfig,
    SystemProfileBody, SystemProfileConfig, SystemSection, TvAssignment, TvConfig, TvConnection,
    TvState, UserProfileConfig, UserSection, references_of,
};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Error type for TV manager operations.
#[derive(Debug, thiserror::Error)]
pub enum TvManagerError {
    /// TV not found in the managed set.
    #[error("TV not found: {0}")]
    TvNotFound(String),

    /// TV exists but is not currently connected.
    #[error("Not connected to TV: {0}")]
    NotConnected(String),

    /// TV is intentionally powered off (no reconnection attempts).
    #[error("TV {0} is powered off")]
    PoweredOff(String),

    /// Wake-on-LAN failed for a TV.
    #[error("Wake failed for TV {tv_id}: {source}")]
    WakeFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// Connection to a TV failed.
    #[error("Connection failed for TV {tv_id}: {source}")]
    ConnectionFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// A command executed on a TV connection failed.
    #[error("Command failed on TV {tv_id}: {source}")]
    CommandFailed {
        tv_id: String,
        source: tisvlay_core::Error,
    },

    /// Power-cycle failure with a typed reason. Carries the
    /// per-domain `PowerError` so consumers (D-Bus error
    /// mapping, GUI inline-error surface, tray) can route on a
    /// stable [`PowerErrorCode`](tisvlay_core::errors::PowerErrorCode)
    /// rather than parsing a free-form string.
    #[error("Power-cycle failure on TV {tv_id}: {kind}")]
    Power {
        tv_id: String,
        #[source]
        kind: tisvlay_core::errors::PowerError,
    },

    /// Pairing failed.
    #[error("Pairing failed: {0}")]
    PairingFailed(Box<dyn std::error::Error + Send + Sync>),

    /// `Manager1.CancelPair(tv_id)` called against a tv_id that isn't
    /// currently pairing (already completed, never started, wrong id).
    /// Strict variant matching the BlueZ `Adapter.RemoveDevice` on
    /// unknown-device convention — clients only call `CancelPair`
    /// when they know a pair is in flight (they're listening to
    /// `PairingProgress` signals); a typed error helps debugging if
    /// that invariant is ever broken.
    #[error("TV {0} is not currently pairing")]
    NotPairing(String),

    /// Configuration I/O error (load/save).
    #[error("Config error: {0}")]
    ConfigError(String),

    /// Profile-CRUD: target profile UUID not in the catalog.
    ///
    /// Distinct from [`TvNotFound`](Self::TvNotFound) because the
    /// D-Bus surface routes the two to different error names; clients
    /// disambiguate "TV gone" from "profile gone" without parsing the
    /// message string.
    #[error("Profile not found: {0}")]
    ProfileNotFound(String),

    /// Profile-CRUD: attempt to delete or otherwise mutate a reserved
    /// profile in a way the daemon doesn't allow (currently: deleting
    /// the reserved `DEFAULT_SYSTEM_PROFILE_UUID`).
    #[error("Reserved profile cannot be modified: {0}")]
    ReservedProfile(String),

    /// Profile-CRUD: a profile body's `name` collides with an existing
    /// profile, is empty, or attempts to claim the reserved
    /// `"default"` literal for a non-default UUID.
    #[error("Profile name invalid: {0}")]
    ProfileNameInvalid(String),

    /// Profile-CRUD: attempt to delete a profile that is still
    /// referenced by at least one TV (`Tv.profile.system` points at
    /// it, or the TV has no explicit assignment and the target is
    /// the reserved default).
    #[error("Profile still referenced by {count} TV(s): {profile_uuid}")]
    ProfileReferenced { profile_uuid: String, count: usize },

    /// Profile-CRUD: persistence failed after the in-memory mutation
    /// already landed. Carries the underlying I/O error message; the
    /// caller is responsible for rolling back the in-memory snapshot
    /// before propagating this variant.
    #[error("I/O error: {0}")]
    Io(String),
}

/// Event emitted when TV state changes or daemon-wide events occur.
///
/// Routed through the state change channel to the forwarder task in main.rs,
/// which updates cached D-Bus properties via `InterfaceRef::get_mut()` and
/// emits `PropertiesChanged` signals.
#[derive(Debug, Clone)]
pub enum StateChangeEvent {
    /// Backend subscription push -- full TV state snapshot.
    ///
    /// The forwarder updates cached properties on all TV interface structs
    /// and emits `PropertiesChanged` for each changed property.
    SubscriptionUpdate { tv_id: String, state: TvState },
    /// Connection status changed (connected or disconnected).
    ///
    /// Emitted after successful connect/reconnect (connected=true) or when
    /// a backend subscription ends (connected=false).
    ConnectionChanged { tv_id: String, connected: bool },
    /// A TV config option was changed (managed, name, profile.system, etc.).
    ///
    /// The forwarder updates the corresponding cached property on TvService
    /// and emits `PropertiesChanged`.
    OptionChanged {
        tv_id: String,
        key: String,
        value: String,
    },
    /// Configuration was reloaded from disk.
    ///
    /// Profile lists, TV names, and managed flags may all differ.
    ConfigReloaded,
    /// A TV's vendor-specific config was edited (`Tv1.UpdateConfig`
    /// touched at least one schema field).
    ///
    /// The forwarder refreshes per-TV cached properties that derive
    /// from the vendor config — `Tv1.VendorInfo` (label/value
    /// projection) and `Tv1.Power.SupportsWake` (LG: depends on
    /// MAC presence). The connection itself drops and reconnects
    /// inside `update_tv_config` so the new transport is picked up
    /// independently; this event handles the cached-property
    /// resync that has no other trigger.
    TvVendorConfigChanged { tv_id: String },
    /// A network interface's carrier went up (cable plugged in, link established).
    ///
    /// From kernel netlink RTMGRP_LINK. Triggers reconnection for TVs
    /// whose `monitored_interface` matches this interface.
    InterfaceUp { interface: String },
    /// A network interface's carrier went down (cable unplugged, link lost).
    ///
    /// From kernel netlink RTMGRP_LINK. Disconnects TVs whose
    /// `monitored_interface` matches this interface.
    InterfaceDown { interface: String },
    /// The kernel routing table changed.
    ///
    /// From kernel netlink RTMGRP_ROUTE. Re-resolves `monitored_interface`
    /// for all connected network TVs in case their route shifted to a
    /// different NIC (failover, DHCP renewal, VPN up/down).
    RouteChanged,
    /// In-process desktop probe produced a new mapping for a TV.
    ///
    /// The forwarder invalidates the `org.tisvlay.Tv1.Desktop` cached
    /// properties so the next read returns the updated identity.
    DesktopMappingChanged { tv_id: String },
    /// A power-on flow walked into a new phase.
    ///
    /// The forwarder emits `Tv1.Power.PowerCycleProgress(phase: s)` so
    /// the GUI can show staged feedback during the 5-30 s wake window.
    PowerCycleProgress {
        tv_id: String,
        phase: tisvlay_core::PowerCyclePhase,
    },
    /// A pairing flow transitioned to a new state.
    ///
    /// The forwarder emits `Manager1.PairingProgress(tv_id, phase: s)`
    /// so wizard UIs can render staged feedback ("Confirm on TV..." /
    /// "Awaiting input" / "Complete" / "Failed: <reason>").
    ///
    /// `phase` matches [`tisvlay_core::PairingState::to_dbus_state`]:
    /// `awaiting_tv`, `awaiting_input`, `complete`, `failed`.
    PairingProgress {
        tv_id: String,
        phase: String,
        message: String,
    },
}

/// Basic TV information for D-Bus properties.
#[derive(Debug, Clone)]
pub struct TvInfo {
    pub name: String,
    pub vendor: String,
    pub managed: bool,
}

/// TV ids that appeared or disappeared during a [`TvManager::reload_config`]
/// call, returned so the dbus layer can register / unregister the
/// corresponding `Tv1.*` object paths in lockstep with the in-memory
/// state. The `Manager1.AddTv` and `Tv1.Remove` flows already keep
/// path lifecycle and manager state symmetric; reload was the only
/// mutation site that diverged.
///
/// The `system_profiles_*` lists carry the per-profile catalog diff
/// for the system bus side. `Manager1` emits `SystemProfileAdded` /
/// `SystemProfileChanged` / `SystemProfileRemoved` signals against
/// these UUIDs after a successful reload so clients with cached
/// profile bodies know to re-fetch. UUIDs are stringified hyphenated
/// to match the wire shape of the corresponding signals.
///
/// The session daemon owns the user-profile catalog diff for the
/// user-config layer and emits its own signals — see
/// `tisvlay-session::session_dbus::reload_config`.
#[derive(Debug, Clone, Default)]
pub struct ReloadDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub system_profiles_added: Vec<String>,
    pub system_profiles_removed: Vec<String>,
    pub system_profiles_changed: Vec<String>,
}

/// Managed TV connection state.
struct ManagedTv {
    /// TV configuration.
    config: TvConfig,
    /// Active connection (None if disconnected).
    connection: Option<Box<dyn TvConnection>>,
    /// TV was intentionally powered off -- don't try to reconnect until power_on.
    powered_off: bool,
    /// A connection attempt is already in progress (WoL retry, on_wake, etc.).
    /// Prevents carrier/route events from racing with an existing attempt.
    reconnecting: bool,
    /// A re-pair handshake is already in progress for this TV. Set true
    /// at the start of `re_pair_tv` (under the per-TV Mutex), reset to
    /// false on completion (success, failure, or cancel). Concurrent
    /// `re_pair_tv` calls for the same TV check this flag and bail with
    /// `TvManagerError::PairingFailed("already pairing")` rather than
    /// running two handshakes against the same TV in parallel.
    ///
    /// Distinct from `reconnecting` because re-pair is a different
    /// flow (closes connection + runs the full pair handshake against
    /// the existing vendor_config + persists updated credentials),
    /// not just a connection-only retry.
    pairing_in_flight: bool,
    /// Resolved system profile for daemon lifecycle events.
    ///
    /// Resolved from the TV's `profile.system` assignment against the
    /// system profiles defined in the config. Updated on config reload
    /// and when the profile assignment changes via `set_tv_option`.
    system_profile: ResolvedSystemProfile,
    /// Last known current input from backend subscription pushes.
    ///
    /// Used by `is_automated()` to evaluate the input-aware gate without
    /// needing access to the D-Bus interface cache.
    last_known_input: Option<String>,
    /// Last known power state from backend subscription pushes —
    /// the raw vendor-reported enum, so the reliable-dispatch
    /// pre-flight can distinguish `ActiveStandby` (NIC alive, panel
    /// dark, transitional) from `ScreenOff` (true panel-off target).
    /// `Unknown` until the first subscription push lands.
    ///
    /// Used by `screen_on` / `screen_off`'s pre-flight wake gate and
    /// verify check to decide whether the TV has reached the target
    /// state, without an IPC round-trip back through the D-Bus
    /// property cache. The D-Bus `Tv1.Power.PowerState` property
    /// derives its string from this same enum via
    /// [`crate::power_state::derive_power_state`].
    last_known_power_state: PowerState,
    /// Broadcasts every update to [`Self::last_known_power_state`].
    ///
    /// Allows reliable-dispatch paths in `screen_on` / `screen_off`
    /// to await an authoritative transition (with timeout) instead
    /// of polling the cached field on a fixed `tokio::time::sleep`
    /// cadence. The subscription task writes the cached field and
    /// fires the sender in the same critical section, so waiters
    /// can't miss a transition that happens between their
    /// `subscribe()` and their first `changed()` await — they see
    /// the current value via `borrow()` before awaiting.
    ///
    /// `watch` (rather than `broadcast`) because the value is
    /// always-current and waiters only care about "is the cached
    /// PowerState a state I want now"; missed intermediate values
    /// are fine.
    power_state_tx: tokio::sync::watch::Sender<PowerState>,
    /// NIC carrying traffic to this TV, resolved from the routing table.
    ///
    /// Set at connection time via `resolve_interface_for_ip()`. Used to
    /// scope carrier monitoring to the correct interface — only disconnect
    /// this TV when its specific NIC goes down, not when any NIC changes.
    monitored_interface: Option<String>,
    /// Whether the active connection pushes state via a backend
    /// subscription (`subscribe_state` succeeded and a forwarder task
    /// is running). Set from the return of
    /// [`TvManager::try_subscribe_state`] at every connect/reconnect
    /// site; `false` for poll-only backends (e.g. an Epson projector
    /// over a request/response transport) that don't implement
    /// `subscribe_state`.
    ///
    /// Two consumers gate on this so push backends (LG, the default
    /// mock) keep their exact prior behavior:
    /// - [`state_poll_loop`] only polls TVs with `false` here — a push
    ///   backend's state already flows through the subscription task.
    /// - `screen_on` / `screen_off` actively call
    ///   [`TvManager::refresh_power_state`] in their verify loop when
    ///   this is `false`, because nothing else fires the
    ///   `power_state_tx` watch the passive verify awaits.
    has_push_subscription: bool,
    /// Desired screen state — the single source of truth for the panel:
    /// `Some(true)` = on, `Some(false)` = off, `None` = no opinion yet
    /// (resets each daemon start). Every direct screen action sets it
    /// (latest-wins); the push-driven [`TvManager::reconcile_screen`]
    /// re-asserts it whenever the TV's `PowerState` changes, so an action
    /// that arrived while the TV was reconnecting still lands once the TV
    /// is reachable.
    desired_screen: Option<bool>,
}

impl ManagedTv {
    fn new(config: TvConfig, system_profile: ResolvedSystemProfile) -> Self {
        let (power_state_tx, _) = tokio::sync::watch::channel(PowerState::Unknown);
        Self {
            config,
            connection: None,
            powered_off: false,
            reconnecting: false,
            pairing_in_flight: false,
            system_profile,
            last_known_input: None,
            last_known_power_state: PowerState::Unknown,
            power_state_tx,
            monitored_interface: None,
            has_push_subscription: false,
            desired_screen: None,
        }
    }

    /// Whether automation should fire for this TV right now.
    ///
    /// Composes `managed`, `automate_when` (from system profile), `primary_input`
    /// (from config), and `last_known_input` (from subscription pushes).
    fn is_automated(&self, managed: bool) -> bool {
        if !managed {
            return false;
        }
        self.system_profile.automate_when.should_automate(
            self.config.primary_input.as_deref(),
            self.last_known_input.as_deref(),
        )
    }

    fn is_connected(&self) -> bool {
        self.connection
            .as_ref()
            .map(|c| c.is_connected())
            .unwrap_or(false)
    }

    fn capabilities(&self) -> Capabilities {
        self.connection
            .as_ref()
            .map(|c| c.capabilities())
            .unwrap_or(Capabilities::empty())
    }
}

/// Map value for a single TV in `TvManager::tvs`.
///
/// The `bool` is the `managed` flag stored outside the per-TV mutex so that
/// `list_managed_tv_ids` can scan it with only the outer read lock.
type TvEntry = (bool, Arc<Mutex<ManagedTv>>);

/// TV connection manager.
///
/// Thread-safe manager for multiple TV connections.
/// Uses per-TV locking so operations on different TVs proceed concurrently.
/// Connection resilience (auto-reconnect, WoL retry) is handled by the backends.
pub struct TvManager {
    /// Path to the configuration file for load/save operations.
    config_path: PathBuf,
    /// Backend registry for vendor support.
    registry: Arc<BackendRegistry>,
    /// Managed TVs by ID -- each TV has its own mutex for independent locking.
    /// The outer RwLock protects the IndexMap itself (add/remove); individual TV
    /// operations hold only the per-TV Mutex. IndexMap preserves insertion order
    /// so list_tvs returns a stable, deterministic list matching the config file order.
    ///
    /// The `bool` is the `managed` flag, stored outside the per-TV mutex so that
    /// `list_managed_tv_ids` can scan it with only the outer read lock — no per-TV
    /// lock acquisition, no risk of blocking behind a slow `power_on`.
    tvs: Arc<RwLock<IndexMap<String, TvEntry>>>,
    /// System-level config section (system profiles for daemon lifecycle events).
    system_section: RwLock<SystemSection>,
    /// User-level config section (user profiles for session event defaults).
    user_section: RwLock<UserSection>,
    /// Serializes all load-modify-save disk operations (save_tv_config, delete_tv_config,
    /// set_tv_option). Without this, two concurrent callers can each load a stale snapshot
    /// and one overwrites the other's change on disk.
    disk_write_lock: Mutex<()>,
    /// Channel to send state change events.
    state_change_tx: mpsc::UnboundedSender<StateChangeEvent>,
    /// In-memory cache of desktop mappings produced by the in-process
    /// DRM-EDID probe (see [`crate::desktop_probe`]). Repopulated on
    /// every TV `ConnectionChanged(true)` and on `Manager1.ReloadConfig`.
    /// Backs the `org.tisvlay.Tv1.Desktop` D-Bus surface so tray / GUI /
    /// CLI consumers read from a single place. Not persisted to disk —
    /// re-derives deterministically from sysfs each daemon start.
    desktop_mappings: RwLock<std::collections::HashMap<String, tisvlay_core::DesktopMapping>>,
    /// Command senders for in-flight `PairingSession`s, keyed by tv_id.
    /// Registered at the start of `pair_tv` / `re_pair_tv` (so external
    /// dispatch via `Manager1.CancelPair(tv_id)` can reach the
    /// session's `PairingCommand::Cancel` channel), removed on
    /// completion (success, failure, or cancel).
    ///
    /// The map is the bridge between the D-Bus surface (which knows
    /// tv_ids) and the in-stack-frame `PairingSession`s owned by
    /// `pair_tv`'s and `re_pair_tv`'s async tasks. Without this map
    /// the session is unreachable from outside its owning scope; the
    /// daemon-side `PairingSession::cancel()` exists but had no D-Bus
    /// surface to invoke it until the re-pair plan landed.
    ///
    /// `parking_lot::Mutex` because critical sections are brief
    /// (insert / lookup-and-clone / remove) and never span an
    /// `.await` point. Holding it across await would risk deadlock
    /// against the per-TV `Mutex<ManagedTv>`. parking_lot also has
    /// no poison state, so a panic in some unrelated task can never
    /// take down the daemon through this lock.
    pairing_sessions: parking_lot::Mutex<
        std::collections::HashMap<String, mpsc::Sender<tisvlay_core::PairingCommand>>,
    >,
}

impl std::fmt::Debug for TvManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TvManager")
            .field("config_path", &self.config_path)
            .finish_non_exhaustive()
    }
}

/// RAII guard that removes a `pairing_sessions` entry on `Drop`.
///
/// `run_pairing_handshake` inserts a session's command-sender into
/// the map so `Manager1.CancelPair(tv_id)` can reach it. The entry
/// must be removed when the handshake ends — on normal Ok/Err
/// return, on panic-unwind through the await, **and** if the
/// surrounding future is dropped mid-handshake (e.g., the daemon
/// shuts down). A manual `pairing_sessions.lock().remove(tv_id)` at
/// the end of the function would handle the Ok/Err cases but
/// silently leak the entry on future-drop. The guard's `Drop` impl
/// covers every path.
struct PairingSessionGuard<'a> {
    pairing_sessions: &'a parking_lot::Mutex<
        std::collections::HashMap<String, mpsc::Sender<tisvlay_core::PairingCommand>>,
    >,
    tv_id: String,
}

impl Drop for PairingSessionGuard<'_> {
    fn drop(&mut self) {
        self.pairing_sessions.lock().remove(&self.tv_id);
    }
}

/// Result of [`TvManager::await_power_state`].
///
/// `Matched` — the predicate matched (either immediately on the
/// cached value or on a later transition observed via the watch
/// sender). `TimedOut` — the bounded timeout fired (or the watch
/// sender dropped due to TV removal) without a match; the carried
/// `PowerState` is the most recently observed cached state for
/// caller-side logging or fallback logic.
///
/// Both variants carry a `PowerState` so callers always have a
/// value to log; the discriminant is the success/timeout signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AwaitOutcome {
    Matched(PowerState),
    TimedOut(PowerState),
}

/// System lifecycle event type.
///
/// Dispatched per-TV from each TV's resolved system profile by the
/// `logind` monitor, the daemon startup path, the DPMS watcher, and the
/// greeter idle client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    Startup,
    Shutdown,
    Reboot,
    Sleep,
    Wake,
    DpmsOff,
    DpmsOn,
    /// Login greeter went idle (from `tisvlay-greeter` over `Manager1`).
    GreeterIdle,
    /// Login greeter activity resumed.
    GreeterResume,
}

/// Apply a key-value TV option to a `TvConfig`.
///
/// Used by `set_tv_option` to update both the disk config and the in-memory
/// config with a single shared field-mapping, so neither can drift from the other.
fn apply_tv_option(tv: &mut TvConfig, key: &str, value: &str) -> Result<(), TvManagerError> {
    match key {
        "primary_input" => {
            tv.primary_input = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "managed" => {
            tv.managed = value
                .parse()
                .map_err(|_| TvManagerError::ConfigError("Invalid bool".into()))?;
        }
        "name" => {
            tv.name = value.to_string();
        }
        "profile.system" => {
            tv.profile.system = value.parse::<uuid::Uuid>().map_err(|_| {
                TvManagerError::ConfigError(format!("Invalid system profile UUID: {value}"))
            })?;
        }
        _ => {
            return Err(TvManagerError::ConfigError(format!(
                "Unknown TV option: {}",
                key
            )));
        }
    }
    Ok(())
}

/// Ingest one `TvState` snapshot into the daemon's caches and fan it
/// out to the D-Bus property forwarder.
///
/// Updates the in-process cached fields on `ManagedTv` that the
/// reliable-dispatch helper and automation gate read
/// (`last_known_input`, `last_known_power_state`), fires the
/// `power_state_tx` watch so verify waiters wake, then sends a
/// `StateChangeEvent::SubscriptionUpdate` so `update_tv_cached_state`
/// refreshes the cached D-Bus properties + emits `PropertiesChanged`.
///
/// Shared by [`run_subscription_task`] (push backends) and
/// [`state_poll_loop`] (poll-only backends) so a polled snapshot lands
/// exactly like a pushed one. Returns `false` if the forwarder channel
/// has closed, so the push task can stop iterating.
async fn forward_state_snapshot(
    tv_id: &str,
    tvs: &Arc<RwLock<IndexMap<String, TvEntry>>>,
    tx: &mpsc::UnboundedSender<StateChangeEvent>,
    state: TvState,
) -> bool {
    // Update cached fields on ManagedTv that the daemon's
    // reliable-dispatch helper and automation gate read in-process.
    // `last_known_power_state` carries the raw vendor PowerState so
    // the pre-flight wake gate can distinguish ActiveStandby from a
    // true ScreenOff steady state; the D-Bus property emit collapses
    // to a string via `crate::power_state::derive_power_state`.
    {
        let tvs_read = tvs.read().await;
        if let Some((_, tv_arc)) = tvs_read.get(tv_id) {
            let mut tv = tv_arc.lock().await;
            if let Some(ref input) = state.current_input {
                tv.last_known_input = Some(input.clone());
            }
            tv.last_known_power_state = state.power_state;
            // Fire the watch so reliable-dispatch paths waiting on a
            // transition wake up. `send_replace` ignores the "no
            // receivers" case (no one is currently waiting — fine; the
            // next subscriber sees this value via `borrow()`).
            tv.power_state_tx.send_replace(state.power_state);
        }
    }
    tx.send(StateChangeEvent::SubscriptionUpdate {
        tv_id: tv_id.to_string(),
        state,
    })
    .is_ok()
}

/// Body of the task spawned by [`TvManager::try_subscribe_state`].
///
/// Forwards backend state pushes as `StateChangeEvent::SubscriptionUpdate`
/// until the backend's `state_rx` closes. When it closes, emit a
/// `ConnectionChanged(false)` only if the TV is actually disconnected
/// — a replacement connection (WoL retry, reconnect, carrier change)
/// starts a fresh subscription whose rx closes this older task's
/// rx out, and a spurious `false` here would tear down any per-TV
/// resources (D-Bus interfaces, downstream observers) that no
/// subsequent event would re-create.
///
/// Extracted as a free function (rather than an inline closure) so
/// its end-of-stream gating is unit-testable without a full
/// `TvManager` + backend stack.
async fn run_subscription_task(
    tv_id: String,
    tvs: Arc<RwLock<IndexMap<String, TvEntry>>>,
    tx: mpsc::UnboundedSender<StateChangeEvent>,
    mut state_rx: mpsc::UnboundedReceiver<TvState>,
) {
    while let Some(state) = state_rx.recv().await {
        if !forward_state_snapshot(&tv_id, &tvs, &tx, state).await {
            break;
        }
    }

    let still_connected = {
        let tvs_read = tvs.read().await;
        let Some((_, tv_arc)) = tvs_read.get(tv_id.as_str()) else {
            // TV removed mid-lifecycle (config reload, RemoveTv D-Bus);
            // its D-Bus objects are gone too, so any emission here
            // would land on a zombie path.
            debug!(tv_id = %tv_id, "Backend state subscription ended (TV unknown)");
            return;
        };
        tv_arc.lock().await.is_connected()
    };
    if !still_connected {
        let _ = tx.send(StateChangeEvent::ConnectionChanged {
            tv_id: tv_id.clone(),
            connected: false,
        });
    }
    debug!(
        tv_id = %tv_id,
        still_connected,
        "Backend state subscription ended"
    );
}

/// Persistent background task that periodically refreshes state for
/// connected poll-only TVs.
///
/// Spawned once at daemon startup alongside the state-change
/// forwarder. Push backends (LG, the default mock) flow state through
/// their `subscribe_state` task in real time and are skipped here — so
/// this loop changes nothing for them. Poll-only backends (e.g. an
/// Epson projector over a request/response transport) have no push
/// channel, so without this loop their live state (power, volume,
/// input, the projector `warming_up` / `cooling_down` substates) would
/// never reach D-Bus clients after the initial connect snapshot.
///
/// Each tick calls [`TvManager::poll_state_once`], which snapshots the
/// TV ids under the read lock and then, per id, queries any connected
/// poll-only TV and forwards the result exactly like a pushed update.
pub async fn state_poll_loop(manager: Arc<TvManager>) {
    info!(
        interval_secs = STATE_POLL_INTERVAL.as_secs(),
        "Poll-only state refresh loop started"
    );
    let mut ticker = tokio::time::interval(STATE_POLL_INTERVAL);
    // Skip the immediate first tick `interval` fires at t=0: a
    // just-connected poll-only TV already has its connect snapshot, so
    // the first useful poll is one interval later.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        manager.poll_state_once().await;
    }
}

impl TvManager {
    /// Create a new TV manager from configuration.
    ///
    /// Returns the manager and a receiver for state change events.
    pub fn new(
        config: SystemConfig,
        config_path: PathBuf,
        registry: Arc<BackendRegistry>,
    ) -> (Self, mpsc::UnboundedReceiver<StateChangeEvent>) {
        // Use IndexMap to preserve config file insertion order so GetTvs
        // returns a stable, deterministic list across restarts.
        let mut tvs = IndexMap::new();
        for tv_config in &config.tv {
            let system_profile = config.resolve_system_profile(tv_config);
            tvs.insert(
                tv_config.uuid.to_string(),
                (
                    tv_config.managed,
                    Arc::new(Mutex::new(ManagedTv::new(
                        tv_config.clone(),
                        system_profile,
                    ))),
                ),
            );
        }

        let tvs = Arc::new(RwLock::new(tvs));
        let (state_change_tx, state_change_rx) = mpsc::unbounded_channel();
        let system_section = RwLock::new(config.system.clone());
        let user_section = RwLock::new(config.user.clone());

        let manager = Self {
            config_path,
            registry,
            tvs,
            system_section,
            user_section,
            disk_write_lock: Mutex::new(()),
            state_change_tx,
            desktop_mappings: RwLock::new(std::collections::HashMap::new()),
            pairing_sessions: parking_lot::Mutex::new(std::collections::HashMap::new()),
        };

        (manager, state_change_rx)
    }

    /// Resolved on-disk path of the daemon's config file. Surfaced as
    /// the `Manager1.ConfigPath` D-Bus property so clients can show
    /// the actual location regardless of `--config` overrides.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Emit a state change event.
    /// Try to subscribe to backend state updates for a TV.
    ///
    /// If the backend supports push-based state (e.g., LG SSAP subscriptions),
    /// spawns a background task that forwards state updates as StateChangeEvents
    /// and returns `true`. If the backend doesn't support subscriptions, returns
    /// `false` without spawning anything.
    ///
    /// The returned bool is stored on [`ManagedTv::has_push_subscription`]
    /// (via [`Self::refresh_subscription`]) so the daemon can tell push
    /// backends (poll skipped, passive verify) apart from poll-only backends
    /// (the [`state_poll_loop`] refreshes their state; `screen_on` /
    /// `screen_off` actively verify).
    ///
    /// The spawned forwarder reads back from `self.tvs` to apply
    /// `last_known_input` updates, so subscribing *before* the TV is inserted
    /// into the map is safe: its `if let Some(...)` guard skips the update for
    /// a not-yet-present entry and keeps forwarding the state event.
    async fn try_subscribe_state(&self, tv_id: &str, connection: &mut dyn TvConnection) -> bool {
        match connection.subscribe_state().await {
            Ok(state_rx) => {
                info!(tv_id = %tv_id, "Subscribed to backend state updates");
                tokio::spawn(run_subscription_task(
                    tv_id.to_string(),
                    self.tvs.clone(),
                    self.state_change_tx.clone(),
                    state_rx,
                ));
                true
            }
            Err(_) => {
                // Backend doesn't push state (poll-only, e.g. a projector
                // over a request/response transport). `state_poll_loop`
                // periodically refreshes such TVs via `get_state`, and the
                // reliable-dispatch verify loop actively queries power state
                // instead of awaiting a push that never arrives.
                false
            }
        }
    }

    /// Subscribe to backend state pushes (if supported) and store the
    /// result on the TV. A thin wrapper over [`Self::try_subscribe_state`]
    /// for the call sites that install a fresh connection.
    async fn refresh_subscription(&self, tv_id: &str, tv: &mut ManagedTv) {
        tv.has_push_subscription = if let Some(conn) = tv.connection.as_mut() {
            self.try_subscribe_state(tv_id, conn.as_mut()).await
        } else {
            false
        };
    }

    /// Get a clone of the state change sender for external event sources.
    pub fn state_change_sender(&self) -> mpsc::UnboundedSender<StateChangeEvent> {
        self.state_change_tx.clone()
    }

    /// Run a single state-refresh pass over all connected poll-only TVs.
    ///
    /// One iteration of [`state_poll_loop`], factored out so tests can
    /// drive a deterministic poll without waiting on the
    /// [`STATE_POLL_INTERVAL`] timer.
    ///
    /// Snapshots the TV ids under the read lock, then per id: locks the
    /// TV and — only if it is **connected** and has **no push
    /// subscription** — queries `get_state` wrapped in
    /// [`STATE_POLL_QUERY_TIMEOUT`] so a slow/reconnecting projector
    /// can't hold the per-TV `Mutex` (and block concurrent `Tv1.*`
    /// calls) for longer than that. On success the snapshot is ingested
    /// exactly like a pushed update. On error or timeout the TV is
    /// skipped (the backend self-marks disconnected; the next pass skips
    /// it via the connected check).
    ///
    /// Push backends (LG, the default mock) are never polled here — the
    /// `has_push_subscription` gate is what keeps their behavior
    /// identical to before this loop existed.
    pub async fn poll_state_once(&self) {
        let tv_ids: Vec<String> = {
            let tvs = self.tvs.read().await;
            tvs.keys().cloned().collect()
        };

        for tv_id in &tv_ids {
            let Ok(tv_arc) = self.get_tv(tv_id).await else {
                continue;
            };

            // Hold the per-TV lock across the query so the snapshot
            // can't race a concurrent reconnect swapping the connection
            // out. The timeout bounds how long that lock is held.
            let snapshot = {
                let mut tv = tv_arc.lock().await;
                // Skip push backends (their subscription already pushes
                // state) and disconnected TVs (nothing to query).
                if tv.has_push_subscription || !tv.is_connected() {
                    continue;
                }
                let Some(conn) = tv.connection.as_mut() else {
                    continue;
                };
                match tokio::time::timeout(STATE_POLL_QUERY_TIMEOUT, conn.get_state()).await {
                    Ok(Ok(state)) => Some(state),
                    Ok(Err(e)) => {
                        debug!(tv_id = %tv_id, error = %e, "Poll-only state query failed; skipping");
                        None
                    }
                    Err(_elapsed) => {
                        debug!(tv_id = %tv_id, "Poll-only state query timed out; skipping");
                        None
                    }
                }
            };

            if let Some(state) = snapshot {
                forward_state_snapshot(tv_id, &self.tvs, &self.state_change_tx, state).await;
            }
        }
    }

    /// Resolve and store the NIC carrying traffic to a TV.
    ///
    /// Parses the TV's display address as an IP, queries the kernel routing
    /// table, and stores the result on `ManagedTv::monitored_interface`.
    /// Only meaningful for network-transport TVs.
    async fn resolve_monitored_interface(&self, tv_id: &str) {
        let tv_arc = match self.get_tv(tv_id).await {
            Ok(arc) => arc,
            Err(_) => return,
        };
        let config = {
            let tv = tv_arc.lock().await;
            tv.config.clone()
        };

        if !self.registry.transport_for(&config).is_network_dependent() {
            return;
        }

        let Some(address) = self.registry.primary_descriptor(&config) else {
            debug!(tv_id = %tv_id, "Backend exposed no primary descriptor for route resolution");
            return;
        };
        let ip: std::net::IpAddr = match address.parse() {
            Ok(ip) => ip,
            Err(_) => {
                debug!(tv_id = %tv_id, address = %address, "Cannot parse TV address as IP for route resolution");
                return;
            }
        };

        let interface = crate::netlink::resolve_interface_for_ip(ip).await;
        let mut tv = tv_arc.lock().await;
        if tv.monitored_interface == interface {
            return;
        }

        let previous = tv.monitored_interface.clone();
        info!(tv_id = %tv_id, "Monitored interface: {previous:?} -> {interface:?}");
        tv.monitored_interface = interface.clone();

        // Should we tear down the existing connection?
        let should_disconnect = !tv.reconnecting
            && match (&previous, &interface) {
                // Route disappeared — TV is unreachable.
                (Some(_), None) => true,
                // Route shifted to a different NIC — close if the protocol
                // can't survive a source IP change (e.g., WebSocket over TCP).
                (Some(_), Some(_)) => {
                    tv.is_connected() && !self.registry.survives_source_ip_change(&config)
                }
                _ => false,
            };

        // Should we signal reconnection?
        let should_reconnect =
            interface.is_some() && !tv.is_connected() && !tv.powered_off && !tv.reconnecting;

        // Take connection outside the lock if disconnecting.
        let conn = if should_disconnect {
            tv.last_known_input = None;
            tv.connection.take()
        } else {
            None
        };

        drop(tv);

        if let Some(mut conn) = conn {
            let _ = conn.close().await;
            info!(tv_id = %tv_id, "Route changed ({previous:?} -> {interface:?}): disconnected");
            self.emit_connection_changed(tv_id, false);
        }

        if (should_disconnect || should_reconnect)
            && let Some(iface) = interface
        {
            self.emit_state_change(StateChangeEvent::InterfaceUp { interface: iface });
        }
    }

    fn emit_state_change(&self, event: StateChangeEvent) {
        if let Err(e) = self.state_change_tx.send(event) {
            debug!("Failed to send state change event: {}", e);
        }
    }

    /// Shorthand for the 7+ call sites that emit a connection-status
    /// change. Avoids re-spelling the event shape each time and keeps
    /// the construction in one place so future field additions don't
    /// have to chase every call site.
    fn emit_connection_changed(&self, tv_id: &str, connected: bool) {
        self.emit_state_change(StateChangeEvent::ConnectionChanged {
            tv_id: tv_id.to_string(),
            connected,
        });
    }

    /// Get a per-TV Arc, briefly holding the outer read lock for lookup only.
    async fn get_tv(&self, tv_id: &str) -> Result<Arc<Mutex<ManagedTv>>, TvManagerError> {
        self.tvs
            .read()
            .await
            .get(tv_id)
            .map(|(_, arc)| arc.clone())
            .ok_or_else(|| TvManagerError::TvNotFound(tv_id.to_string()))
    }

    /// Get the system action for a specific TV and event.
    ///
    /// Reads the TV's resolved system profile to determine the action.
    /// Returns `SessionAction::None` if the TV is not found.
    async fn get_system_action(&self, tv_id: &str, event: SystemEvent) -> SessionAction {
        let tv_arc = match self.get_tv(tv_id).await {
            Ok(arc) => arc,
            Err(_) => return SessionAction::None,
        };
        let tv = tv_arc.lock().await;
        match event {
            SystemEvent::Startup => tv.system_profile.on_startup,
            SystemEvent::Shutdown => tv.system_profile.on_shutdown,
            SystemEvent::Reboot => tv.system_profile.on_reboot,
            SystemEvent::Sleep => tv.system_profile.on_sleep,
            SystemEvent::Wake => tv.system_profile.on_wake,
            SystemEvent::DpmsOff => tv.system_profile.on_dpms_off,
            SystemEvent::DpmsOn => tv.system_profile.on_dpms_on,
            // Proactive greeter blanking is opt-in per-TV via a non-zero
            // `idle_timeout_seconds`: the greeter fires one fan-out event for
            // the whole compositor, so gate the *idle* action here too —
            // otherwise a TV that never opted in (timeout 0, but the default
            // `on_idle_timeout` is `screen_off`) would be blanked whenever
            // another TV's timeout elapses.
            SystemEvent::GreeterIdle if tv.system_profile.idle_timeout_seconds > 0 => {
                tv.system_profile.on_idle_timeout
            }
            SystemEvent::GreeterIdle => SessionAction::None,
            // Resume/presence is *not* gated on the idle timer: a greeter
            // resume — including the client's presence-assert at startup — must
            // re-light an automated TV the prior session latched dark, even when
            // that TV opts out of proactive blanking. The automation gate in
            // `execute_system_event_for_tv` already scopes this to managed TVs;
            // `on_idle_resume` defaults to `screen_on`.
            SystemEvent::GreeterResume => tv.system_profile.on_idle_resume,
        }
    }

    /// Shortest greeter idle timeout (seconds) across the TVs that have it
    /// enabled, or 0 when none do.
    ///
    /// The login greeter has a single idle clock but system profiles are
    /// per-TV, so the greeter client registers one notification at the
    /// shortest enabled timeout; the daemon then fans the resulting
    /// greeter-idle/resume out per-TV. A TV counts only if it has a non-zero
    /// `idle_timeout_seconds` AND a real `on_idle_timeout` action — otherwise
    /// it would pull the single clock shorter without ever acting. TVs that do
    /// share the clock still blank at the shortest value (a longer-timeout TV
    /// fires early).
    ///
    /// This is a deliberate parallel of the *session* idle pipeline
    /// (`tisvlay_session::SessionState::effective_idle_timeout` + its
    /// `IdleTimeout`/`IdleResume` → `on_idle_timeout`/`on_idle_resume` fan-out):
    /// same single-shared-clock-plus-per-TV-action shape, but over the system
    /// profile / greeter rather than the user profile / logged-in session, in a
    /// different crate. They share no code (different profile types, sync map
    /// vs async-locked registry); keep the shared-clock semantics in sync, or
    /// unify them behind a generic idle pipeline if a third consumer appears.
    pub async fn greeter_idle_timeout_seconds(&self) -> u64 {
        let tvs = self.tvs.read().await;
        let mut min: Option<u64> = None;
        for (_, (managed, tv_arc)) in tvs.iter() {
            let tv = tv_arc.lock().await;
            if !tv.is_automated(*managed) {
                continue;
            }
            let p = &tv.system_profile;
            if p.idle_timeout_seconds > 0 && p.on_idle_timeout != SessionAction::None {
                min = Some(min.map_or(p.idle_timeout_seconds, |m| m.min(p.idle_timeout_seconds)));
            }
        }
        min.unwrap_or(0)
    }

    /// List TV IDs where automation should fire (managed + input-aware gate).
    async fn list_automated_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        let mut result = Vec::new();
        for (id, (managed, tv_arc)) in tvs.iter() {
            let tv = tv_arc.lock().await;
            if tv.is_automated(*managed) {
                result.push(id.clone());
            }
        }
        result
    }

    /// Whether a single TV passes the automation gate (managed + input-aware).
    async fn is_tv_automated(&self, tv_id: &str) -> bool {
        let tvs = self.tvs.read().await;
        match tvs.get(tv_id) {
            Some((managed, tv_arc)) => tv_arc.lock().await.is_automated(*managed),
            None => false,
        }
    }

    /// Execute a system event on all automated TVs using each TV's resolved system profile.
    pub async fn execute_system_event(&self, event: SystemEvent, context: &str) {
        let tv_ids = self.list_automated_tv_ids().await;
        for tv_id in &tv_ids {
            self.execute_system_event_for_tv(tv_id, event, context)
                .await;
        }
    }

    /// Execute a system event for one specific TV.
    ///
    /// The DPMS watcher calls this directly: it knows exactly which TV's
    /// connector changed, so dispatch is scoped rather than fanned out
    /// across every automated TV. No-op when the TV fails the automation
    /// gate or its resolved action is `None`.
    pub async fn execute_system_event_for_tv(
        &self,
        tv_id: &str,
        event: SystemEvent,
        context: &str,
    ) {
        if !self.is_tv_automated(tv_id).await {
            return;
        }
        let action = self.get_system_action(tv_id, event).await;
        if action == SessionAction::None {
            return;
        }

        let result = self.apply_action(action, tv_id).await;

        match result {
            Ok(()) => {
                debug!(tv_id = %tv_id, action = ?action, context = context, "Action executed");
            }
            Err(e) => {
                warn!(tv_id = %tv_id, action = ?action, context = context, error = %e,
                    "Failed to execute action");
            }
        }
    }

    /// Connect to all configured TVs without executing startup actions.
    ///
    /// Used on warm restart (daemon process restart, not system boot).
    /// The TVs are likely already on — just re-establish WebSocket
    /// connections, subscribe to state updates, and resolve monitored
    /// interfaces. No WoL, no screen_on, no input switching.
    fn connect_all_on_restart(self: Arc<Self>) {
        tokio::spawn(async move {
            let tv_ids = self.list_tv_ids().await;
            for tv_id in &tv_ids {
                let display_addr = {
                    let tv_arc = match self.get_tv(tv_id).await {
                        Ok(arc) => arc,
                        Err(_) => continue,
                    };
                    let tv = tv_arc.lock().await;
                    self.registry.primary_descriptor(&tv.config)
                };

                info!(tv_id = %tv_id, address = ?display_addr, "Warm restart: connecting to TV");

                match self.reconnect(tv_id).await {
                    Ok(()) => info!(tv_id = %tv_id, "Warm restart: connected"),
                    Err(e) => {
                        debug!(tv_id = %tv_id, error = %e, "Warm restart: connection failed (TV may be off)")
                    }
                }
            }
        });
    }

    /// Execute the configured `on_startup` action for all managed TVs,
    /// but only on system boot — not on daemon restart.
    ///
    /// Uses a marker file in `/run/tisvlay/` (tmpfs, cleared on every boot)
    /// to distinguish system boot from service restart. This prevents
    /// unexpected TV power-on during unrelated `systemctl restart`.
    ///
    /// Runs in the background so D-Bus registration is not blocked.
    /// The daemon is reachable immediately; TVs come online when ready.
    ///
    /// Each managed TV is handled according to its own resolved system profile:
    /// - `PowerOn`: WoL + connect + screen on + HDMI input switch
    /// - Other actions: dispatched directly
    /// - `None`: skipped
    pub fn on_startup(self: &Arc<Self>) {
        const BOOT_MARKER: &str = "/run/tisvlay/booted";

        let marker_path = std::path::Path::new(BOOT_MARKER);
        if marker_path.exists() {
            info!("Skipping on_startup actions (service restart, not system boot)");
            // Still connect to all TVs — they're likely already on from
            // before the restart. No WoL, no screen_on, no input switch.
            self.clone().connect_all_on_restart();
            return;
        }

        // Create the marker for subsequent restarts.
        // Failure is non-fatal but means on_startup will fire on every restart.
        if let Some(dir) = marker_path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            warn!(error = %e, "Failed to create boot marker directory — on_startup will fire on every restart");
        }
        if let Err(e) = std::fs::write(marker_path, "") {
            warn!(error = %e, "Failed to write boot marker — on_startup will fire on every restart");
        }

        let manager = self.clone();
        tokio::spawn(async move {
            info!("Executing on_startup per-TV actions");

            // Collect per-TV startup actions and configs for TVs that need WoL.
            let tv_ids = manager.list_managed_tv_ids().await;

            // First pass: handle PowerOn TVs (WoL + connect flow).
            for tv_id in &tv_ids {
                let action = manager.get_system_action(tv_id, SystemEvent::Startup).await;
                if action != SessionAction::PowerOn {
                    continue;
                }

                // Read config under per-TV lock.
                let tv_config = {
                    let tv_arc = match manager.get_tv(tv_id).await {
                        Ok(arc) => arc,
                        Err(_) => continue,
                    };
                    let mut tv = tv_arc.lock().await;
                    if !manager.registry.supports_wake(&tv.config) {
                        warn!(tv_id = %tv_id, "Startup PowerOn: TV does not support wake, skipping");
                        continue;
                    }
                    // Mark the wake in progress so a concurrent screen action
                    // (e.g. a greeter screen_off) defers and records intent
                    // instead of racing this wake with its own power_on.
                    tv.reconnecting = true;
                    tv.config.clone()
                };

                info!(tv_id = %tv_id, "Waking TV and waiting for connection...");
                let connection = match manager.registry.wake_and_connect_tv(&tv_config).await {
                    Ok(conn) => {
                        info!(tv_id = %tv_id, "Startup: TV is online");
                        conn
                    }
                    Err(e) => {
                        warn!(tv_id = %tv_id, error = %e, "Startup: TV did not come online after wake");
                        if let Ok(tv_arc) = manager.get_tv(tv_id).await {
                            tv_arc.lock().await.reconnecting = false;
                        }
                        continue;
                    }
                };

                // Store the connection and subscribe to state updates, clearing
                // the wake-in-progress flag now that we're connected.
                {
                    let tvs = manager.tvs.read().await;
                    if let Some((_, tv_arc)) = tvs.get(tv_id.as_str()) {
                        let mut tv = tv_arc.lock().await;
                        tv.connection = Some(connection);
                        tv.powered_off = false;
                        tv.reconnecting = false;
                        manager.refresh_subscription(tv_id, &mut tv).await;
                    }
                }
                manager.emit_connection_changed(tv_id, true);
                manager.resolve_monitored_interface(tv_id).await;

                // Wake the panel — unless a presence source already requested
                // it off while we were booting (an away user's greeter idle).
                // That latched `desired_screen = Some(false)` is left for
                // reconcile to apply, so the panel stays dark.
                let want_screen_on = match manager.get_tv(tv_id).await {
                    Ok(arc) => arc.lock().await.desired_screen != Some(false),
                    Err(_) => true,
                };
                if want_screen_on {
                    match manager.screen_on(tv_id).await {
                        Ok(()) => info!(tv_id = %tv_id, "Startup: screen on"),
                        Err(e) => {
                            warn!(tv_id = %tv_id, error = %e, "Startup: failed to turn screen on")
                        }
                    }
                } else {
                    info!(tv_id = %tv_id, "Startup: leaving panel off (presence requested screen off)");
                }

                if let Some(ref input) = tv_config.primary_input {
                    match manager.switch_input(tv_id, input).await {
                        Ok(()) => info!(tv_id = %tv_id, input = %input, "Startup: switched input"),
                        Err(e) => {
                            warn!(tv_id = %tv_id, error = %e, "Startup: failed to switch input")
                        }
                    }
                }
            }

            // Second pass: handle non-PowerOn actions (ScreenOn, ScreenOff, PowerOff).
            for tv_id in &tv_ids {
                let action = manager.get_system_action(tv_id, SystemEvent::Startup).await;
                match action {
                    SessionAction::None | SessionAction::PowerOn => continue, // already handled or skip
                    _ => {}
                }

                // None / PowerOn are filtered above; apply_action handles
                // the remaining ScreenOff / ScreenOn / PowerOff.
                let result = manager.apply_action(action, tv_id).await;

                match result {
                    Ok(()) => {
                        debug!(tv_id = %tv_id, action = ?action, "Startup action executed");
                    }
                    Err(e) => {
                        warn!(tv_id = %tv_id, action = ?action, error = %e,
                            "Startup: failed to execute action");
                    }
                }
            }
        });
    }

    /// Get the admin baseline for user profiles.
    ///
    /// Returns the body at [`DEFAULT_USER_PROFILE_UUID`] from the system
    /// config, or an empty `UserProfileConfig` if not defined.
    ///
    /// [`DEFAULT_USER_PROFILE_UUID`]: tisvlay_core::DEFAULT_USER_PROFILE_UUID
    pub async fn get_user_profile_default(&self) -> UserProfileConfig {
        let user = self.user_section.read().await;
        user.profiles
            .get(&tisvlay_core::DEFAULT_USER_PROFILE_UUID)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all user profiles as (name, UserProfileConfig) pairs. The
    /// name comes from the body — profiles are UUID-keyed internally
    /// in the catalog; this projection feeds the name-keyed wire
    /// methods.
    pub async fn list_user_profiles(&self) -> Vec<(String, UserProfileConfig)> {
        let user = self.user_section.read().await;
        user.profiles
            .values()
            .map(|v| (v.name.clone(), v.clone()))
            .collect()
    }

    /// Get all system profiles as (name, SystemProfileConfig) pairs.
    /// Same name-keyed wire shape as [`list_user_profiles`](Self::list_user_profiles).
    pub async fn list_system_profiles(&self) -> Vec<(String, tisvlay_core::SystemProfileConfig)> {
        let system = self.system_section.read().await;
        system
            .profiles
            .values()
            .map(|v| (v.name.clone(), v.clone()))
            .collect()
    }

    // -------------------------------------------------------------------------
    // System-profile CRUD
    //
    // The four mutators (`create_system_profile`, `duplicate_system_profile`,
    // `update_system_profile`, `delete_system_profile`) follow the same
    // shape: validate under the write lock, mutate in-memory, persist via
    // `spawn_blocking`, and roll the in-memory mutation back to its
    // pre-mutation snapshot if persistence fails. Persistence is gated
    // by `disk_write_lock` so concurrent CRUD calls serialize against
    // each other and against `set_tv_option` / `update_tv_config`
    // load-modify-save cycles.
    //
    // None of these emit D-Bus signals directly — the `Manager1`
    // handlers do that after a successful return so the catalog is
    // guaranteed fetchable by the moment a client receives `Added` /
    // `Changed` / `Removed`.
    // -------------------------------------------------------------------------

    /// Snapshot of the system-profile catalog suitable for
    /// `Manager1.ListSystemProfiles -> a{sa{sv}}`. Returns `(uuid,
    /// body)` pairs so the wire encoder can map UUIDs to strings
    /// without re-locking.
    pub async fn list_system_profile_bodies(&self) -> Vec<(Uuid, SystemProfileBody)> {
        let system = self.system_section.read().await;
        system
            .profiles
            .iter()
            .map(|(uuid, body)| (*uuid, SystemProfileBody::from_config(body)))
            .collect()
    }

    /// Fetch one system-profile body by UUID. Returns
    /// [`TvManagerError::ProfileNotFound`] if the UUID isn't in the
    /// catalog.
    pub async fn get_system_profile_body(
        &self,
        uuid: Uuid,
    ) -> Result<SystemProfileBody, TvManagerError> {
        let system = self.system_section.read().await;
        system
            .profiles
            .get(&uuid)
            .map(SystemProfileBody::from_config)
            .ok_or_else(|| TvManagerError::ProfileNotFound(uuid.to_string()))
    }

    /// Hard-coded baseline that every system profile inherits from
    /// when its body has all fields `None`. Wire-side mirror of
    /// [`ResolvedSystemProfile::default()`] expressed as a
    /// [`SystemProfileBody`] (name = [`DEFAULT_PROFILE_NAME`], every
    /// other field `Some(default)`).
    pub fn resolved_system_profile_defaults() -> SystemProfileBody {
        SystemProfileBody::from_config(&SystemProfileConfig {
            name: DEFAULT_PROFILE_NAME.to_string(),
            ..SystemProfileConfig::full_default()
        })
    }

    /// Resolve the full inheritance walk for a TV's assigned system
    /// profile and return the result as a [`SystemProfileBody`] —
    /// every field present, suitable for `Manager1.GetEffectiveSystemProfile`.
    ///
    /// The walker is `SystemSection::resolve_profile_by_uuid` (which
    /// already encodes the per-TV → default → hardcoded chain); this
    /// wrapper just projects the resolved values back into a body.
    pub async fn get_effective_system_profile(
        &self,
        tv_id: &str,
    ) -> Result<SystemProfileBody, TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;
        let assigned_uuid = {
            let tv = tv_arc.lock().await;
            tv.config.profile.system
        };
        let resolved = {
            let system = self.system_section.read().await;
            system.resolve_profile_by_uuid(assigned_uuid)
        };
        // Surface the assigned profile's display name when available
        // (clients usually know it client-side, but emitting it here
        // keeps the wire body self-describing). Fall back to the
        // reserved default name for unknown UUIDs.
        let display_name = {
            let system = self.system_section.read().await;
            system
                .profiles
                .get(&assigned_uuid)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| DEFAULT_PROFILE_NAME.to_string())
        };
        Ok(SystemProfileBody {
            name: display_name,
            on_startup: Some(resolved.on_startup),
            on_shutdown: Some(resolved.on_shutdown),
            on_reboot: Some(resolved.on_reboot),
            on_sleep: Some(resolved.on_sleep),
            on_wake: Some(resolved.on_wake),
            on_dpms_off: Some(resolved.on_dpms_off),
            on_dpms_on: Some(resolved.on_dpms_on),
            idle_timeout_seconds: Some(resolved.idle_timeout_seconds),
            on_idle_timeout: Some(resolved.on_idle_timeout),
            on_idle_resume: Some(resolved.on_idle_resume),
            automate_when: Some(resolved.automate_when),
        })
    }

    /// Create a new system profile with the given display name.
    ///
    /// Validates `name` (non-empty, not the reserved `"default"`
    /// literal, unique against existing catalog body names), mints a
    /// fresh UUID, inserts an empty (`SystemProfileConfig::default`)
    /// body under the new UUID, and persists. Returns the new UUID
    /// on success.
    pub async fn create_system_profile(&self, name: String) -> Result<Uuid, TvManagerError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(TvManagerError::ProfileNameInvalid(
                "name must be non-empty".to_string(),
            ));
        }
        if trimmed == DEFAULT_PROFILE_NAME {
            return Err(TvManagerError::ProfileNameInvalid(format!(
                "\"{DEFAULT_PROFILE_NAME}\" is reserved for the default profile"
            )));
        }

        // Hold `disk_write_lock` for the whole validate→mutate→persist
        // window so concurrent CRUD callers (or `set_tv_option`)
        // can't observe a partially-committed state.
        let _disk_lock = self.disk_write_lock.lock().await;
        let new_uuid = Uuid::new_v4();
        let new_body = SystemProfileConfig {
            name: trimmed.to_string(),
            ..SystemProfileConfig::default()
        };

        {
            let mut system = self.system_section.write().await;
            // Uniqueness check across the existing catalog.
            if system.profiles.values().any(|p| p.name == trimmed) {
                return Err(TvManagerError::ProfileNameInvalid(format!(
                    "name \"{trimmed}\" already in use"
                )));
            }
            // The minted UUID is fresh from `Uuid::new_v4`; collision
            // probability is effectively zero, but guard against the
            // theoretical case so we never silently overwrite a body.
            if system.profiles.contains_key(&new_uuid) {
                return Err(TvManagerError::ConfigError(
                    "UUID collision on new profile (retry)".to_string(),
                ));
            }
            system.profiles.insert(new_uuid, new_body);
        } // section write lock released before disk I/O

        // Persist; roll back the in-memory insert on I/O failure.
        match self.persist_full_config().await {
            Ok(()) => {
                info!(
                    profile_uuid = %new_uuid,
                    name = %trimmed,
                    "System profile created"
                );
                Ok(new_uuid)
            }
            Err(e) => {
                let mut system = self.system_section.write().await;
                system.profiles.remove(&new_uuid);
                Err(e)
            }
        }
    }

    /// Duplicate an existing system profile under a new display name.
    ///
    /// Same validation rules as [`create_system_profile`](Self::create_system_profile),
    /// but the new body inherits every field (besides `name`) from
    /// the source.
    pub async fn duplicate_system_profile(
        &self,
        source_uuid: Uuid,
        new_name: String,
    ) -> Result<Uuid, TvManagerError> {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return Err(TvManagerError::ProfileNameInvalid(
                "name must be non-empty".to_string(),
            ));
        }
        if trimmed == DEFAULT_PROFILE_NAME {
            return Err(TvManagerError::ProfileNameInvalid(format!(
                "\"{DEFAULT_PROFILE_NAME}\" is reserved for the default profile"
            )));
        }

        let _disk_lock = self.disk_write_lock.lock().await;
        let new_uuid = Uuid::new_v4();

        {
            let mut system = self.system_section.write().await;
            let source = system
                .profiles
                .get(&source_uuid)
                .ok_or_else(|| TvManagerError::ProfileNotFound(source_uuid.to_string()))?;
            if system.profiles.values().any(|p| p.name == trimmed) {
                return Err(TvManagerError::ProfileNameInvalid(format!(
                    "name \"{trimmed}\" already in use"
                )));
            }
            if system.profiles.contains_key(&new_uuid) {
                return Err(TvManagerError::ConfigError(
                    "UUID collision on new profile (retry)".to_string(),
                ));
            }
            let mut cloned = source.clone();
            cloned.name = trimmed.to_string();
            system.profiles.insert(new_uuid, cloned);
        }

        match self.persist_full_config().await {
            Ok(()) => {
                info!(
                    profile_uuid = %new_uuid,
                    source_uuid = %source_uuid,
                    name = %trimmed,
                    "System profile duplicated"
                );
                Ok(new_uuid)
            }
            Err(e) => {
                let mut system = self.system_section.write().await;
                system.profiles.remove(&new_uuid);
                Err(e)
            }
        }
    }

    /// Delete a system profile.
    ///
    /// Rejects the reserved [`DEFAULT_SYSTEM_PROFILE_UUID`] outright;
    /// rejects when at least one TV still references the profile
    /// (either explicitly via `Tv.profile.system` or implicitly via
    /// the default-fan-out rule from `references_of`). Persistence
    /// failure rolls the in-memory removal back.
    pub async fn delete_system_profile(&self, uuid: Uuid) -> Result<(), TvManagerError> {
        if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
            return Err(TvManagerError::ReservedProfile(uuid.to_string()));
        }

        let _disk_lock = self.disk_write_lock.lock().await;

        // Reference check: walk every TV's assignment via `references_of`
        // so the implicit-default fan-out rule matches the client-side
        // check (deleting a non-default UUID never falls into the
        // implicit-default branch, so `references_of` returns only
        // explicit references here; we still call it to keep the
        // semantics one-source-of-truth).
        let referencing = {
            let assignments = self.tv_assignments_for_refs().await;
            references_of(uuid, &assignments)
        };
        if !referencing.is_empty() {
            return Err(TvManagerError::ProfileReferenced {
                profile_uuid: uuid.to_string(),
                count: referencing.len(),
            });
        }

        // Take a snapshot of the removed body so we can re-insert on
        // I/O failure (preserves the catalog's pre-mutation state).
        let removed_body = {
            let mut system = self.system_section.write().await;
            match system.profiles.remove(&uuid) {
                Some(b) => b,
                None => return Err(TvManagerError::ProfileNotFound(uuid.to_string())),
            }
        };

        match self.persist_full_config().await {
            Ok(()) => {
                info!(profile_uuid = %uuid, "System profile deleted");
                Ok(())
            }
            Err(e) => {
                let mut system = self.system_section.write().await;
                system.profiles.insert(uuid, removed_body);
                Err(e)
            }
        }
    }

    /// Atomic full-body replace for an existing system profile.
    ///
    /// Validates `body.name` (non-empty, unique across the catalog
    /// excluding the same UUID, default-name reserved for the
    /// default UUID), overwrites the catalog entry wholesale via
    /// `SystemProfileConfig::from(&body)`, persists. On I/O failure
    /// the previous body is restored.
    pub async fn update_system_profile(
        &self,
        uuid: Uuid,
        body: SystemProfileBody,
    ) -> Result<(), TvManagerError> {
        let trimmed = body.name.trim();
        if trimmed.is_empty() {
            return Err(TvManagerError::ProfileNameInvalid(
                "name must be non-empty".to_string(),
            ));
        }
        if uuid == DEFAULT_SYSTEM_PROFILE_UUID {
            // Reserved-default UUID must always render as
            // `DEFAULT_PROFILE_NAME`; the daemon's inheritance walker
            // and the GUI's `(default)` badge rely on it.
            if trimmed != DEFAULT_PROFILE_NAME {
                return Err(TvManagerError::ProfileNameInvalid(format!(
                    "default profile's name must be \"{DEFAULT_PROFILE_NAME}\""
                )));
            }
        } else if trimmed == DEFAULT_PROFILE_NAME {
            return Err(TvManagerError::ProfileNameInvalid(format!(
                "\"{DEFAULT_PROFILE_NAME}\" is reserved for the default profile"
            )));
        }

        let _disk_lock = self.disk_write_lock.lock().await;

        // Build the new config under the write lock so the
        // uniqueness check observes the same snapshot the swap-in
        // operates against.
        let prior_body = {
            let mut system = self.system_section.write().await;
            if !system.profiles.contains_key(&uuid) {
                return Err(TvManagerError::ProfileNotFound(uuid.to_string()));
            }
            // Uniqueness excludes the entry being updated itself.
            if system
                .profiles
                .iter()
                .any(|(other_uuid, body)| *other_uuid != uuid && body.name == trimmed)
            {
                return Err(TvManagerError::ProfileNameInvalid(format!(
                    "name \"{trimmed}\" already in use"
                )));
            }
            let mut new_config = SystemProfileConfig::from(&body);
            new_config.name = trimmed.to_string();
            system
                .profiles
                .insert(uuid, new_config)
                .expect("contains_key guarded the entry; insert must return Some")
        };

        match self.persist_full_config().await {
            Ok(()) => {
                info!(
                    profile_uuid = %uuid,
                    name = %trimmed,
                    "System profile updated"
                );
                // Re-resolve every TV's cached system profile in case
                // the body change shifts inheritance for TVs that point
                // at this UUID (or, when `uuid` is the default, every
                // TV with no explicit assignment).
                let system = self.system_section.read().await;
                let tvs = self.tvs.read().await;
                for (_, tv_arc) in tvs.values() {
                    let mut tv = tv_arc.lock().await;
                    tv.system_profile = system.resolve_profile_by_uuid(tv.config.profile.system);
                }
                Ok(())
            }
            Err(e) => {
                let mut system = self.system_section.write().await;
                system.profiles.insert(uuid, prior_body);
                Err(e)
            }
        }
    }

    /// Collect lightweight per-TV profile assignments suitable for
    /// `references_of` walks. Reads only the outer `tvs` map + each
    /// per-TV lock briefly; never touches the network.
    async fn tv_assignments_for_refs(&self) -> Vec<TvAssignment> {
        let tvs = self.tvs.read().await;
        let mut out = Vec::with_capacity(tvs.len());
        for (id, (_, tv_arc)) in tvs.iter() {
            let parsed = match Uuid::parse_str(id) {
                Ok(u) => u,
                // Defensive: tv ids are always UUIDs, but a bad ID
                // shouldn't poison the whole walk.
                Err(_) => continue,
            };
            let tv = tv_arc.lock().await;
            out.push(TvAssignment {
                tv_uuid: parsed,
                system_profile: Some(tv.config.profile.system),
                user_profile: None,
            });
        }
        out
    }

    /// Load the on-disk config, replace the `[system]` section from
    /// the current in-memory state, save it back atomically.
    ///
    /// Used by every system-profile CRUD mutator. Holding
    /// `disk_write_lock` upstream is the caller's responsibility — this
    /// helper never grabs it itself so the lock can span the
    /// in-memory mutation + persistence window without nesting.
    async fn persist_full_config(&self) -> Result<(), TvManagerError> {
        let config_path = self.config_path.clone();
        let system_snapshot = self.system_section.read().await.clone();
        tokio::task::spawn_blocking(move || -> Result<(), TvManagerError> {
            let mut config = SystemConfig::load_from(&config_path)
                .map_err(|e| TvManagerError::Io(format!("Failed to load config: {e}")))?;
            config.system = system_snapshot;
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::Io(format!("Failed to save config: {e}")))
        })
        .await
        .map_err(|e| TvManagerError::Io(format!("Task join error: {e}")))?
    }

    /// Get the system profile UUID assigned to a TV, string-formatted.
    pub async fn get_tv_system_profile(&self, tv_id: &str) -> Option<String> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.config.profile.system.to_string())
    }

    /// Get the resolved `automate_when` policy for a TV.
    pub async fn get_tv_automate_when(&self, tv_id: &str) -> Option<tisvlay_core::AutomateWhen> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.system_profile.automate_when)
    }

    /// Get the primary input for a TV (from config).
    pub async fn get_tv_primary_input(&self, tv_id: &str) -> Option<String> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        tv.config.primary_input.clone()
    }

    /// List all configured TV IDs.
    pub async fn list_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        tvs.keys().cloned().collect()
    }

    /// Get basic TV information.
    pub async fn get_tv_info(&self, tv_id: &str) -> Option<TvInfo> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(TvInfo {
            name: tv.config.name.clone(),
            vendor: tv.config.vendor.clone(),
            managed: tv.config.managed,
        })
    }

    /// List IDs of TVs that participate in automated management.
    pub async fn list_managed_tv_ids(&self) -> Vec<String> {
        let tvs = self.tvs.read().await;
        tvs.iter()
            .filter_map(|(id, (managed, _))| if *managed { Some(id.clone()) } else { None })
            .collect()
    }

    /// User-editable vendor-config rows (LG: IP, MAC). Read at
    /// `Tv1.*` interface registration time and cached on
    /// `Tv1.VendorConfig`. Empty when the backend has no
    /// `UserInput` schema fields or the TV isn't found.
    pub async fn vendor_config(&self, tv_id: &str) -> Vec<(String, String)> {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => {
                let tv = tv_arc.lock().await;
                self.registry.display_config(&tv.config)
            }
            Err(_) => Vec::new(),
        }
    }

    /// Backend-discovered diagnostic rows (LG: WebSocket scheme).
    /// Read at `Tv1.*` interface registration time and cached on
    /// `Tv1.VendorInfo`; refreshed via `TvVendorConfigChanged` after
    /// `Tv1.UpdateConfig` writes. Empty when the backend has no
    /// `Diagnostic` schema fields or the TV isn't found.
    pub async fn vendor_diagnostics(&self, tv_id: &str) -> Vec<(String, String)> {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => {
                let tv = tv_arc.lock().await;
                self.registry.display_diagnostics(&tv.config)
            }
            Err(_) => Vec::new(),
        }
    }

    /// Whether this TV's vendor backend can wake the TV from
    /// standby (e.g. has a MAC for Wake-on-LAN). Read at D-Bus
    /// interface registration time and cached on
    /// `Tv1.Power.SupportsWake`; the GUI uses it to gate the
    /// Power-on button when the TV is offline AND can't be woken.
    pub async fn supports_wake(&self, tv_id: &str) -> bool {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => {
                let tv = tv_arc.lock().await;
                self.registry.supports_wake(&tv.config)
            }
            Err(_) => false,
        }
    }

    /// Check if a TV is connected.
    pub async fn is_connected(&self, tv_id: &str) -> bool {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => {
                let tv = tv_arc.lock().await;
                tv.is_connected()
            }
            Err(_) => false,
        }
    }

    /// Whether a reconnect / wake-on-LAN is already in flight for this TV.
    ///
    /// The DPMS watcher consults this before dispatching a display-wake
    /// action so a poll edge can't kick off a second `power_on` while one
    /// is mid-flight (`power_on` does not self-serialize).
    pub async fn is_reconnecting(&self, tv_id: &str) -> bool {
        match self.get_tv(tv_id).await {
            Ok(tv_arc) => tv_arc.lock().await.reconnecting,
            Err(_) => false,
        }
    }

    /// Fetch the current `TvState` from a connected TV's live backend.
    ///
    /// Returns `None` if the TV is not connected or the backend
    /// `get_state()` call fails. Used by the D-Bus registration path
    /// to populate interface properties with real values instead of
    /// hardcoded defaults.
    pub async fn get_current_state(&self, tv_id: &str) -> Option<TvState> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let mut tv = tv_arc.lock().await;
        let conn = tv.connection.as_mut()?;
        conn.get_state().await.ok()
    }

    /// Get TV capabilities.
    pub async fn get_capabilities(&self, tv_id: &str) -> Option<Capabilities> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        Some(tv.capabilities())
    }

    /// Get the TV's vendor-reported hardware identity, if known.
    ///
    /// Returns `None` when the TV isn't currently connected or the
    /// backend doesn't report identity. The backend's own `hardware_id`
    /// method is synchronous — it reads a cached value — so this
    /// doesn't hold the connection lock across any I/O.
    pub async fn get_hardware_id(&self, tv_id: &str) -> Option<tisvlay_core::HardwareId> {
        let tv_arc = self.get_tv(tv_id).await.ok()?;
        let tv = tv_arc.lock().await;
        tv.connection.as_ref().and_then(|c| c.hardware_id())
    }

    /// Get the current desktop mapping for a TV.
    ///
    /// Returns `None` when the in-process DRM-EDID probe (see
    /// [`crate::desktop_probe`]) hasn't matched a connector yet — e.g.
    /// the backend hasn't connected, the TV's `HardwareId` is empty,
    /// or no DRM connector EDID matches it.
    pub async fn get_desktop_mapping(&self, tv_id: &str) -> Option<tisvlay_core::DesktopMapping> {
        let mappings = self.desktop_mappings.read().await;
        mappings.get(tv_id).cloned()
    }

    /// Record a desktop mapping for a TV.
    ///
    /// Idempotent: when the new mapping is byte-equal to the current
    /// cached value, the entry isn't touched and no event fires.
    /// Otherwise the cache is updated and
    /// `StateChangeEvent::DesktopMappingChanged` fires so the D-Bus
    /// forwarder invalidates the `Tv1.Desktop` property cache.
    /// Callers in the daemon's probe path can re-push every TV's
    /// mapping every probe pass without spamming D-Bus listeners.
    pub async fn set_desktop_mapping(&self, tv_id: &str, mapping: tisvlay_core::DesktopMapping) {
        {
            let mut mappings = self.desktop_mappings.write().await;
            if mappings.get(tv_id) == Some(&mapping) {
                return;
            }
            mappings.insert(tv_id.to_string(), mapping);
        }
        self.emit_state_change(StateChangeEvent::DesktopMappingChanged {
            tv_id: tv_id.to_string(),
        });
    }

    /// Force reconnection to a TV.
    ///
    /// Disconnects (if connected), then reconnects. The per-TV lock is
    /// released before the `connect_tv()` call so that shutdown
    /// (`close_all_connections`) and carrier events can proceed without
    /// blocking on a slow/unreachable backend. Same pattern as
    /// `power_on()` — see its doc comment for the rationale.
    pub async fn reconnect(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;

        // Disconnect and read config under per-TV lock, then release.
        let config = {
            let mut tv = tv_arc.lock().await;

            if tv.powered_off {
                return Err(TvManagerError::PoweredOff(tv_id.to_string()));
            }
            if tv.reconnecting {
                return Err(TvManagerError::ConnectionFailed {
                    tv_id: tv_id.to_string(),
                    source: tisvlay_core::Error::backend(
                        &tv.config.vendor,
                        "reconnect already in progress",
                    ),
                });
            }

            tv.reconnecting = true;
            if let Some(mut conn) = tv.connection.take() {
                let _ = conn.close().await;
            }

            tv.config.clone()
        };

        let display_addr = self.registry.primary_descriptor(&config);
        info!(tv_id = %tv_id, address = ?display_addr, vendor = %config.vendor, "Reconnecting to TV");

        // Connect without holding the lock — backends may retry for
        // seconds (LG: 5 attempts × ~10s each).
        let result = self.registry.connect_tv(&config).await;

        // Store result under the lock.
        {
            let mut tv = tv_arc.lock().await;
            tv.reconnecting = false;
            match result {
                Ok(connection) => {
                    info!(tv_id = %tv_id, "Connected to TV");
                    tv.connection = Some(connection);
                    self.refresh_subscription(tv_id, &mut tv).await;
                }
                Err(e) => {
                    return Err(TvManagerError::ConnectionFailed {
                        tv_id: tv_id.to_string(),
                        source: e,
                    });
                }
            }
        }

        self.emit_connection_changed(tv_id, true);
        self.resolve_monitored_interface(tv_id).await;

        Ok(())
    }

    /// Reconnect network-dependent TVs that are currently disconnected.
    ///
    /// Called when network connectivity is restored (e.g., after a network
    /// blip or NIC reset). Reconnects all TVs regardless of managed status —
    /// managed gates automation (actions), not connectivity. Skips non-network
    /// transports (serial, USB, CEC), TVs that are intentionally powered off,
    /// and TVs with an in-flight WoL attempt. Each reconnection runs
    /// concurrently via spawned tasks.
    /// Disconnect TVs whose monitored interface matches the given name.
    ///
    /// Called when a specific NIC's carrier goes down. Only affects TVs
    /// routed through that interface — other TVs are untouched.
    /// Close all TV connections gracefully.
    ///
    /// Called during daemon shutdown so TVs drop their SSAP sessions
    /// immediately. Without this, the TV holds the stale session until
    /// TCP keepalive times out, causing "Connection reset" on warm restart.
    pub async fn close_all_connections(&self) {
        let mut to_close: Vec<(String, Box<dyn TvConnection>)> = Vec::new();
        {
            let tvs = self.tvs.read().await;
            for (tv_id, (_, tv_arc)) in tvs.iter() {
                let mut tv = tv_arc.lock().await;
                if let Some(conn) = tv.connection.take() {
                    to_close.push((tv_id.clone(), conn));
                }
            }
        }
        for (tv_id, mut conn) in to_close {
            debug!(tv_id = %tv_id, "Closing connection");
            let _ = conn.close().await;
        }
    }

    pub async fn disconnect_tvs_on_interface(&self, interface: &str) {
        let mut to_close: Vec<(String, Box<dyn TvConnection>)> = Vec::new();

        {
            let tvs = self.tvs.read().await;
            for (tv_id, (_, tv_arc)) in tvs.iter() {
                let mut tv = tv_arc.lock().await;
                if tv.monitored_interface.as_deref() != Some(interface) {
                    continue;
                }
                if tv.reconnecting {
                    debug!(tv_id = %tv_id, "Skipping disconnect, WoL already in progress");
                    continue;
                }
                if let Some(conn) = tv.connection.take() {
                    tv.last_known_input = None;
                    to_close.push((tv_id.clone(), conn));
                }
            }
        }

        if to_close.is_empty() {
            debug!(interface = %interface, "No TVs monitored on this interface");
        }
        for (tv_id, mut conn) in to_close {
            let _ = conn.close().await;
            info!(tv_id = %tv_id, "Carrier lost on {interface}: disconnected");
            self.emit_connection_changed(&tv_id, false);
        }
    }

    /// Reconnect TVs whose monitored interface matches the given name.
    ///
    /// Called when a specific NIC's carrier comes back up. Only attempts
    /// reconnection for TVs that were routed through that interface.
    pub async fn reconnect_tvs_on_interface(self: &Arc<Self>, interface: &str) {
        let tvs = self.tvs.read().await;
        for (tv_id, (_, tv_arc)) in tvs.iter() {
            let tv = tv_arc.lock().await;
            if tv.monitored_interface.as_deref() != Some(interface) {
                continue;
            }
            if tv.is_connected() || tv.powered_off || tv.reconnecting {
                continue;
            }
            let tv_id = tv_id.clone();
            let iface = interface.to_owned();
            let manager = Arc::clone(self);
            drop(tv);
            tokio::spawn(async move {
                info!(tv_id = %tv_id, "Carrier restored on {iface}: reconnecting");
                if let Err(e) = manager.reconnect(&tv_id).await {
                    debug!(tv_id = %tv_id, error = %e, "Reconnect failed (TV may be off)");
                }
            });
        }
    }

    /// Re-resolve `monitored_interface` for all connected network TVs.
    ///
    /// Called when the kernel routing table changes. If a TV's route shifted
    /// to a different NIC (failover, DHCP renewal), updates the mapping so
    /// carrier monitoring tracks the correct interface.
    pub async fn re_resolve_all_interfaces(&self) {
        let tv_ids: Vec<String> = {
            let tvs = self.tvs.read().await;
            tvs.keys().cloned().collect()
        };
        for tv_id in &tv_ids {
            self.resolve_monitored_interface(tv_id).await;
        }
    }

    /// Execute a command on a TV connection.
    ///
    /// Connects if not already connected, then executes the command.
    /// Only holds the per-TV mutex -- other TVs are not blocked.
    /// Connection resilience (stale connection retry) is handled by
    /// the backend, not here.
    async fn with_connection<F, T>(&self, tv_id: &str, f: F) -> Result<T, TvManagerError>
    where
        F: FnOnce(
            &mut dyn TvConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = tisvlay_core::Result<T>> + Send + '_>,
        >,
    {
        let tv_arc = self.get_tv(tv_id).await?;
        let mut tv = tv_arc.lock().await;

        // Connect if needed (under the same lock to prevent TOCTOU)
        if !tv.is_connected() {
            if tv.powered_off {
                return Err(TvManagerError::PoweredOff(tv_id.to_string()));
            }

            let display_addr = self.registry.primary_descriptor(&tv.config);
            info!(tv_id = %tv_id, address = ?display_addr, vendor = %tv.config.vendor, "Connecting to TV");

            let connection = self.registry.connect_tv(&tv.config).await.map_err(|e| {
                TvManagerError::ConnectionFailed {
                    tv_id: tv_id.to_string(),
                    source: e,
                }
            })?;

            info!(tv_id = %tv_id, "Connected to TV");
            tv.connection = Some(connection);
            self.emit_connection_changed(tv_id, true);

            // Resolve which NIC routes to this TV (inline — we hold the lock)
            if self
                .registry
                .transport_for(&tv.config)
                .is_network_dependent()
                && let Some(address) = self.registry.primary_descriptor(&tv.config)
                && let Ok(ip) = address.parse::<std::net::IpAddr>()
            {
                let interface = crate::netlink::resolve_interface_for_ip(ip).await;
                if tv.monitored_interface != interface {
                    debug!(tv_id = %tv_id, interface = ?interface, "Monitored interface resolved");
                    tv.monitored_interface = interface;
                }
            }

            // Subscribe to backend state updates if supported
            self.refresh_subscription(tv_id, &mut tv).await;
        }

        let connection = tv
            .connection
            .as_mut()
            .ok_or_else(|| TvManagerError::NotConnected(tv_id.to_string()))?;

        f(connection.as_mut()).await.map_err(|e| {
            warn!(tv_id = %tv_id, error = %e, "Command failed");
            TvManagerError::CommandFailed {
                tv_id: tv_id.to_string(),
                source: e,
            }
        })
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Power on a TV — probe-first, with WoL fallback.
    ///
    /// Picks one of two paths based on whether the TV is reachable:
    ///
    /// - **Direct connect** (TV already up; we just lost the WS):
    ///   skip the magic packet entirely, go straight to connect.
    ///   Emits `DirectConnect` → `Connecting` → `Connected`. Common
    ///   case after a NIC flap, route change, or backgrounded TV
    ///   that's still in `Active`.
    /// - **Wake-on-LAN** (TV in deep standby): magic packet, then
    ///   the backend's connect-retry loop. Emits `WolSent` →
    ///   `Connecting` → `Connected`.
    ///
    /// The probe is "did `connect()` succeed within
    /// [`DIRECT_CONNECT_PROBE_TIMEOUT`]?" — backends own the
    /// transport-specific retry timing, so a fast-failing connect
    /// here means "TV not reachable at all", and we fall through to
    /// the wake path. The timeout caps misbehaving networks
    /// (firewall blackholes etc.) so the user doesn't pay the
    /// kernel's TCP/TLS handshake-stall budget.
    ///
    /// Skips the probe entirely when `was_powered_off` is set:
    /// the user just hit Power Off seconds ago and we *know* the
    /// TV is in standby, so a probe would always fail and burn
    /// the timeout for nothing. Goes straight to WoL.
    ///
    /// If neither path is available (no wake method AND not
    /// reachable), surfaces a typed
    /// [`PowerError::NoMacConfigured`] so the GUI's inline-error
    /// surface can render an actionable message.
    pub async fn power_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;

        let (config, supports_wake, was_powered_off) = {
            let mut tv = tv_arc.lock().await;
            let supports_wake = self.registry.supports_wake(&tv.config);
            let was_powered_off = tv.powered_off;
            let display_addr = self.registry.primary_descriptor(&tv.config);
            info!(
                tv_id = %tv_id,
                address = ?display_addr,
                supports_wake,
                was_powered_off,
                "PowerOn requested",
            );

            // Drop any stale connection and clear the powered-off flag
            tv.powered_off = false;
            tv.reconnecting = true;
            if let Some(mut conn) = tv.connection.take() {
                let _ = conn.close().await;
            }

            (tv.config.clone(), supports_wake, was_powered_off)
        };

        // Heuristic: skip the direct-connect probe when we *know*
        // the TV is in standby. The user just hit Power Off seconds
        // ago — a probe is guaranteed to fail and would just burn
        // the connect timeout. Only skip when we have a wake method;
        // otherwise we'd return immediately with no recovery path.
        let should_probe = !(was_powered_off && supports_wake);

        // Probe direct-connect first (when `should_probe`). Wrapped
        // in a 2 s timeout so misbehaving networks (firewall
        // blackholes, hung TLS handshakes) can't burn the kernel's
        // multi-second handshake-stall budget. Healthy probes
        // complete in <500 ms; the timeout is belt-and-suspenders.
        let direct_result = if should_probe {
            self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::DirectConnect);
            match tokio::time::timeout(
                DIRECT_CONNECT_PROBE_TIMEOUT,
                self.registry.connect_tv(&config),
            )
            .await
            {
                Ok(result) => result.map_err(|e| format!("{e}")),
                Err(_elapsed) => Err(format!(
                    "probe timed out after {:?}",
                    DIRECT_CONNECT_PROBE_TIMEOUT
                )),
            }
        } else {
            debug!(tv_id = %tv_id, "Skipping direct-connect probe; TV was just powered off");
            Err("powered_off hint; skipping probe".to_string())
        };

        let connection = match direct_result {
            Ok(conn) => {
                self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::Connecting);
                debug!(tv_id = %tv_id, "Direct connect succeeded; skipping WoL");
                conn
            }
            Err(probe_err) => {
                debug!(
                    tv_id = %tv_id,
                    error = %probe_err,
                    "Direct-connect probe skipped/failed; falling back to WoL"
                );
                if !supports_wake {
                    // No wake method AND TV not reachable. Without
                    // this, the only evidence the user gets is a
                    // D-Bus method `Err` return — buried in journald
                    // by the GUI's `warn!`. Emitting `Failed` lets
                    // the phase consumer transition out of the
                    // in-flight spinner; the typed `PowerError`
                    // routes through the GUI's inline-error surface.
                    warn!(
                        tv_id = %tv_id,
                        "PowerOn requested but TV is unreachable AND has \
                         no wake method configured (LG: missing MAC; pair \
                         the TV again or edit config to add a MAC)",
                    );
                    self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::Failed);
                    let mut tv = tv_arc.lock().await;
                    tv.reconnecting = false;
                    return Err(TvManagerError::Power {
                        tv_id: tv_id.to_string(),
                        kind: tisvlay_core::errors::PowerError::NoMacConfigured,
                    });
                }

                // Fall through: WoL path. Emit WolSent before kicking
                // off the wake; the backend's WoL send + connect
                // retry loop is atomic from our view, so Connecting
                // fires immediately after.
                self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::WolSent);
                self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::Connecting);
                match self.registry.wake_and_connect_tv(&config).await {
                    Ok(conn) => conn,
                    Err(e) => {
                        let mut tv = tv_arc.lock().await;
                        tv.reconnecting = false;
                        self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::Failed);
                        return Err(TvManagerError::WakeFailed {
                            tv_id: tv_id.to_string(),
                            source: e,
                        });
                    }
                }
            }
        };

        // Store the connection and subscribe to state updates
        {
            let mut tv = tv_arc.lock().await;
            tv.reconnecting = false;
            tv.connection = Some(connection);
            self.refresh_subscription(tv_id, &mut tv).await;
        }

        self.emit_power_cycle_phase(tv_id, tisvlay_core::PowerCyclePhase::Connected);
        self.emit_connection_changed(tv_id, true);
        self.resolve_monitored_interface(tv_id).await;
        info!(tv_id = %tv_id, "TV is online");
        Ok(())
    }

    /// Shorthand for emitting a [`StateChangeEvent::PowerCycleProgress`]
    /// event. Keeps `power_on` readable.
    fn emit_power_cycle_phase(&self, tv_id: &str, phase: tisvlay_core::PowerCyclePhase) {
        self.emit_state_change(StateChangeEvent::PowerCycleProgress {
            tv_id: tv_id.to_string(),
            phase,
        });
    }

    /// Power off a TV.
    ///
    /// After sending the power-off command, clears the daemon's connection
    /// tracking. The backend already closed its own socket.
    pub async fn power_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let result = self
            .with_connection(tv_id, |conn| {
                Box::pin(async move { conn.power_off().await })
            })
            .await;

        // Always drop the connection: if the command failed the socket is
        // likely broken anyway. Only mark as powered-off when the command
        // actually succeeded — if it failed the TV may still be on, and
        // leaving powered_off=false lets auto-reconnect attempt recovery.
        {
            let tv_arc = self.get_tv(tv_id).await?;
            let mut tv = tv_arc.lock().await;
            tv.connection = None;
            // An explicit or system power-off ends any screen intent — the TV
            // is off. Drop the latch so the push-driven reconcile does not
            // later re-assert it (e.g. re-lighting a TV the user just powered
            // off). `screen_on` / `screen_off` re-establish it on the next
            // request. Safe here because no screen path calls `power_off`.
            tv.desired_screen = None;
            if result.is_ok() {
                tv.powered_off = true;
            }
        }

        result
    }

    // -------------------------------------------------------------------------
    // Screen Control
    // -------------------------------------------------------------------------

    /// Await a [`PowerState`] transition that satisfies `predicate`,
    /// bounded by `timeout`.
    ///
    /// The two-arm enum spells intent at every call site:
    /// [`AwaitOutcome::Matched`] is the success case ("predicate
    /// matched on the cached value or on a subsequent transition");
    /// [`AwaitOutcome::TimedOut`] is the timeout / sender-drop case,
    /// carrying the last observed cached state for the caller's log
    /// or fallback logic. The two-tag form replaces an earlier
    /// `Result<PowerState, PowerState>` — homogeneous-type `Result`s
    /// read as "error has state info" which obscures that timeout
    /// is the expected, common path of the graceful-fallback flow.
    ///
    /// The function checks the current cached value before
    /// awaiting, so a transition that already landed before the
    /// call is observed immediately.
    ///
    /// Replaces the previous `tokio::time::sleep(verify_delay)` +
    /// poll pattern with an event-driven wait keyed on the
    /// [`ManagedTv::power_state_tx`] watch sender — subscription
    /// pushes and `refresh_power_state` writes both fire the
    /// channel, so this returns the moment the cached field flips.
    async fn await_power_state(
        &self,
        tv_id: &str,
        timeout: Duration,
        predicate: impl Fn(PowerState) -> bool,
    ) -> AwaitOutcome {
        let mut rx = {
            let Ok(tv_arc) = self.get_tv(tv_id).await else {
                return AwaitOutcome::TimedOut(PowerState::Unknown);
            };
            let tv = tv_arc.lock().await;
            tv.power_state_tx.subscribe()
        };

        let wait = async {
            loop {
                let current = *rx.borrow();
                if predicate(current) {
                    return AwaitOutcome::Matched(current);
                }
                if rx.changed().await.is_err() {
                    // Sender dropped (TV removed); treat as timeout
                    // with last seen.
                    return AwaitOutcome::TimedOut(current);
                }
            }
        };

        match tokio::time::timeout(timeout, wait).await {
            Ok(outcome) => outcome,
            Err(_) => AwaitOutcome::TimedOut(*rx.borrow()),
        }
    }

    /// Refresh the cached [`ManagedTv::last_known_power_state`] with
    /// an authoritative query to the TV, then return the fresh value.
    ///
    /// Closes the subscription-lag window: LG firmware can sit in
    /// a transitional substate for several seconds after a remote
    /// power-off before pushing the state change, so the cached
    /// value alone can mis-report a TV in `ActiveStandby` as still
    /// `Active`. Pre-flight wake decisions in `screen_on` /
    /// `screen_off` route through this so they see the TV's actual
    /// substate at call time.
    ///
    /// Bounded by a tight timeout (1 verify_delay): if the TV is
    /// unresponsive — true `Standby` with the WS dead, or a
    /// half-asleep firmware that won't answer — we treat that as
    /// `Standby` so the pre-flight wakes anyway.
    ///
    /// Never triggers WoL or `power_on`'s wake-and-retry loop. If
    /// `tv.is_connected() == false` at entry we skip the query and
    /// return `Standby` directly so a follow-up `power_on` does the
    /// heavy lifting. Note: when the daemon's `is_connected()` is
    /// true but the underlying backend WS is stale, the backend's
    /// own retry wrapper (e.g. the LG client's `with_retry`) may
    /// reconnect the WebSocket as part of issuing the query — that
    /// reconnect is bounded by the same `timeout` here, so a
    /// half-dead WS doesn't stall the pre-flight indefinitely.
    async fn refresh_power_state(&self, tv_id: &str, timeout: Duration) -> PowerState {
        let tv_arc = match self.get_tv(tv_id).await {
            Ok(arc) => arc,
            Err(_) => return PowerState::Unknown,
        };

        let was_connected = {
            let tv = tv_arc.lock().await;
            tv.is_connected()
        };
        if !was_connected {
            let mut tv = tv_arc.lock().await;
            tv.last_known_power_state = PowerState::Standby;
            tv.power_state_tx.send_replace(PowerState::Standby);
            return PowerState::Standby;
        }

        // Query the existing connection in-place — explicitly not via
        // `with_connection`, which would auto-reconnect (and WoL) if
        // the WS had just closed. For pre-flight we want a read-only
        // probe; a `power_on` follow-up does the heavy work.
        let query = async {
            let mut tv = tv_arc.lock().await;
            let conn = tv.connection.as_mut()?;
            conn.get_power_state().await.ok()
        };
        let fresh = match tokio::time::timeout(timeout, query).await {
            Ok(Some(state)) => state,
            Ok(None) | Err(_) => PowerState::Standby,
        };

        {
            let mut tv = tv_arc.lock().await;
            tv.last_known_power_state = fresh;
            tv.power_state_tx.send_replace(fresh);
        }
        fresh
    }

    /// Turn screen on — inherently reliable: verify cached state
    /// reaches [`PowerState::Active`], retry up to
    /// `backend.verify_retry_config().max_attempts`, escalate to
    /// [`Self::power_on`] (WoL + retry + reconnect) on continued
    /// failure.
    ///
    /// **Pre-flight wake.** If cached
    /// [`ManagedTv::last_known_power_state`] is one of the off-ish
    /// substates per
    /// [`crate::power_state::needs_wake_preflight`] (`Standby` /
    /// `Suspend` / `ActiveStandby` / `Unknown`), skip the verify-retry
    /// loop and go straight to `power_on`. `screen_on` over the
    /// existing WebSocket would either no-op or stall on the LG
    /// client's `REQUEST_MS` waiting for a reply the half-asleep TV
    /// won't send; `ActiveStandby` in particular is a transient
    /// substate where the LG firmware ACKs commands but doesn't
    /// transition to `Active`.
    ///
    /// Costs nothing on the normal lockscreen-wake path (cached
    /// state is `ScreenOff` there, which is *not* in the pre-flight
    /// set); strictly improves worst-case wake time when the TV has
    /// slipped to deeper standby or transitioned through
    /// `ActiveStandby` after a remote-press power-off.
    ///
    /// **Projector warm-up.** If the refreshed state is
    /// [`PowerState::WarmingUp`], the device is already powering on
    /// (laser/lamp warming, no image yet); escalating to `power_on`
    /// would be redundant and report success with a blank screen.
    /// Instead, poll the device to a usable state (`Active` /
    /// `ScreenSaver`), bounded by [`WARMUP_WAIT_TIMEOUT`], then fall
    /// through to the verify-retry loop. Projector-class only — LG
    /// never reports `WarmingUp`.
    ///
    /// **Poll-only verify.** Backends without a push subscription get
    /// an active [`Self::refresh_power_state`] before each passive
    /// verify-await (nothing else fires the `power_state_tx` watch they
    /// await). Push backends keep the passive-await-only path.
    pub async fn screen_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        // Latch the intent so an action deferred (or superseded) here still
        // lands via the push-driven reconcile once the TV is reachable.
        {
            let tv_arc = self.get_tv(tv_id).await?;
            tv_arc.lock().await.desired_screen = Some(true);
        }
        // Defer while a lifecycle wake is in progress: recording the intent
        // and letting the post-wake reconcile apply it avoids a second
        // competing `power_on` racing the wake (the cold-boot tangle).
        if self.is_reconnecting(tv_id).await {
            debug!(
                tv_id,
                "screen_on deferred (reconnecting); reconcile applies on ready"
            );
            return Ok(());
        }
        // Pull retry config and the push/poll discriminator under a
        // lock; the cached state will be refreshed via an authoritative
        // query just below so a stale subscription doesn't decide the
        // pre-flight wrong.
        let (retry_config, has_push) = self.verify_config(tv_id).await?;
        let fresh = self
            .refresh_power_state(tv_id, retry_config.verify_delay)
            .await;

        // Projector already powering on (laser warming, no image yet).
        // Escalating to `power_on` here is redundant and returns
        // success with a blank screen, so instead wait for the device
        // to reach a usable state, then fall through to the normal
        // verify-retry loop (which sends `screen_on` / unmutes and
        // verifies). `WarmingUp` is projector-class only — LG never
        // reports it, so this never engages for push backends.
        if fresh == PowerState::WarmingUp {
            info!(
                tv_id,
                "screen_on: projector warming up; waiting for a usable state before verifying",
            );
            let deadline = tokio::time::Instant::now() + WARMUP_WAIT_TIMEOUT;
            loop {
                // `refresh_power_state` issues a live `get_power_state`
                // and fires `power_state_tx`, so the cached property /
                // D-Bus state track the warm-up progression too.
                let state = self
                    .refresh_power_state(tv_id, retry_config.verify_delay)
                    .await;
                if matches!(state, PowerState::Active | PowerState::ScreenSaver) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    info!(
                        tv_id,
                        cached_power_state = ?state,
                        "screen_on: projector did not finish warming up within the timeout; \
                         proceeding to verify anyway",
                    );
                    break;
                }
                tokio::time::sleep(WARMUP_POLL_INTERVAL).await;
            }
        } else if crate::power_state::needs_wake_preflight(fresh) {
            info!(
                tv_id,
                cached_power_state = ?fresh,
                "screen_on pre-flight: powering on (TV not in a wakeable substate)",
            );
            return self.power_on(tv_id).await;
        }

        for attempt in 0..retry_config.effective_max_attempts() {
            if let Err(e) = self
                .with_connection(tv_id, |conn| {
                    Box::pin(async move { conn.screen_on().await })
                })
                .await
            {
                debug!(tv_id, attempt, error = %e, "screen_on failed; escalating to power_on");
                break;
            }
            // Poll-only backends never push to `power_state_tx`, so the
            // passive await below would always time out. Refresh the
            // cached state with a live query first (which also fires the
            // watch) so the await sees the fresh value. Push backends
            // (LG/mock) skip this — their subscription already updates
            // the cache, so their behavior is unchanged.
            if !has_push {
                self.refresh_power_state(tv_id, retry_config.verify_delay)
                    .await;
            }
            match self
                .await_power_state(tv_id, retry_config.verify_delay, |s| {
                    matches!(s, PowerState::Active | PowerState::ScreenSaver)
                })
                .await
            {
                AwaitOutcome::Matched(_) => return Ok(()),
                AwaitOutcome::TimedOut(last_seen) => {
                    debug!(
                        tv_id,
                        attempt,
                        cached_power_state = ?last_seen,
                        "screen_on did not reach active state",
                    );
                }
            }
        }
        info!(
            tv_id,
            "screen_on escalating to power_on (verify-retry exhausted)",
        );
        self.power_on(tv_id).await
    }

    /// Turn screen off — inherently reliable: verify cached state
    /// reaches a panel-off substate, retry up to
    /// `backend.verify_retry_config().max_attempts`. No escalation
    /// past retries — the TV is already reachable; if it refuses
    /// to blank we log and give up rather than power-cycle.
    ///
    /// **Pre-flight wake.** If cached state is one of the off-ish
    /// substates per [`crate::power_state::needs_wake_preflight`]
    /// (`Standby` / `Suspend` / `ActiveStandby` / `Unknown`), call
    /// [`Self::power_on`] first, then poll cached state until LG
    /// settles into a non-wake-preflight state (post-wake settle
    /// phase). Once settled, fall through to the verify-retry loop
    /// to send the actual `screen_off` command. Serves two cases:
    ///
    /// - `on_startup = "screen_off"` (boot machine with TV powered
    ///   on but panel blanked to avoid overnight burn-in): cached
    ///   starts at `Unknown`, pre-flight wakes the TV.
    /// - User pressed power on the remote, TV transitions through
    ///   `ActiveStandby` (panel dark, NIC alive) before reaching
    ///   `Standby`: an immediate `screen_off` while in
    ///   `ActiveStandby` would otherwise no-op (LG firmware ACKs
    ///   but doesn't change state).
    ///
    /// **Graceful fallback.** If the post-wake settle times out —
    /// LG firmware was honoring a very recent remote-off and
    /// refused to fully wake despite our `power_on` — the TV is
    /// still in an off-ish substate with the panel already dark.
    /// The user's screen_off intent is functionally achieved, so
    /// return `Ok` with an info log rather than burning more
    /// retries on a `turnOffScreen` LG will keep rejecting.
    ///
    /// **Projector cool-down.** If the refreshed state is
    /// [`PowerState::CoolingDown`], the projector is already powering
    /// off and the panel is going dark — the screen-off intent is
    /// satisfied, so return `Ok` rather than escalating to `power_on`
    /// (which would re-light the projector). Projector-class only — LG
    /// never reports `CoolingDown`. This is distinct from `Standby`,
    /// which still routes through the pre-flight wake so the
    /// `on_startup = screen_off` case works.
    ///
    /// **Poll-only verify.** Backends without a push subscription get
    /// an active [`Self::refresh_power_state`] before each passive
    /// verify-await; otherwise the await never sees the transition and
    /// the loop spuriously warns "panel did not blank". Push backends
    /// keep the passive-await-only path.
    ///
    /// Side effect: `on_session_lock = "screen_off"` with the TV
    /// already off briefly powers it on before blanking. Rare but
    /// known; see `docs/lg/firmware-quirks.md` (when ported) for
    /// the design rationale.
    pub async fn screen_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        // Latch the intent so an action deferred (or superseded) here still
        // lands via the push-driven reconcile once the TV is reachable.
        {
            let tv_arc = self.get_tv(tv_id).await?;
            tv_arc.lock().await.desired_screen = Some(false);
        }
        // Defer while a lifecycle wake is in progress: recording the intent
        // and letting the post-wake reconcile apply it avoids a second
        // competing `power_on` racing the wake (the cold-boot tangle).
        if self.is_reconnecting(tv_id).await {
            debug!(
                tv_id,
                "screen_off deferred (reconnecting); reconcile applies on ready"
            );
            return Ok(());
        }
        // Pull retry config and the push/poll discriminator under a
        // lock; the cached state will be refreshed via an authoritative
        // query just below so a stale subscription doesn't decide the
        // pre-flight wrong.
        let (retry_config, has_push) = self.verify_config(tv_id).await?;
        let fresh = self
            .refresh_power_state(tv_id, retry_config.verify_delay)
            .await;

        // Projector deliberately powering off (cooling the light source,
        // panel already going dark). The screen-off intent is already
        // satisfied; escalating to `power_on` would cancel the user's
        // power-off and re-light the projector. `CoolingDown` is
        // projector-class only — LG never reports it — so this is inert
        // for push backends. Note this is checked BEFORE the
        // `needs_wake_preflight` branch (which would otherwise catch
        // `CoolingDown`). `Standby` is intentionally left to that branch
        // so the `on_startup = screen_off` wake case still fires.
        if fresh == PowerState::CoolingDown {
            info!(
                tv_id,
                "screen_off: projector cooling down (powering off); panel already blanking, \
                 accepting as screen-off",
            );
            return Ok(());
        }

        if crate::power_state::needs_wake_preflight(fresh) {
            info!(
                tv_id,
                cached_power_state = ?fresh,
                "screen_off pre-flight: powering on first (TV must reach Active before screen-off substate)",
            );
            self.power_on(tv_id).await?;

            // Post-wake settle: `power_on` returns when the WS
            // reconnects, but LG firmware can keep reporting
            // `ActiveStandby` for a few hundred ms while it
            // transitions to `Active`. Sending `screen_off` in
            // that window gets a 500 rejection. Await the
            // transition out of the wake-preflight set with a
            // bounded timeout.
            // Saturating multiply guards against a pathological
            // `max_attempts` config (usize wider than u32, or a
            // very large value) overflowing `Duration` — clamp to
            // `Duration::MAX` rather than panicking the daemon.
            let attempts = u32::try_from(retry_config.effective_max_attempts()).unwrap_or(u32::MAX);
            let settle_budget = retry_config
                .verify_delay
                .checked_mul(attempts)
                .unwrap_or(Duration::MAX);
            match self
                .await_power_state(tv_id, settle_budget, |s| {
                    !crate::power_state::needs_wake_preflight(s)
                })
                .await
            {
                AwaitOutcome::Matched(PowerState::ScreenOff) => {
                    // LG transitioned straight through to the
                    // target on wake-up — nothing to do.
                    return Ok(());
                }
                AwaitOutcome::Matched(_) => {
                    // Settled into Active / ScreenSaver — fall
                    // through to verify-retry loop.
                }
                AwaitOutcome::TimedOut(_) => {
                    // Settle timed out. LG firmware honored the
                    // recent remote-off and refused to wake into
                    // `Active`; TV sits in `ActiveStandby` (panel
                    // off, NIC alive). The user's screen_off
                    // intent is functionally satisfied (panel is
                    // dark), so accept this as success.
                    info!(
                        tv_id,
                        "screen_off: TV did not settle into Active after power_on; \
                         accepting wake-preflight substate as panel-off (LG firmware quirk)",
                    );
                    return Ok(());
                }
            }
        }

        for attempt in 0..retry_config.effective_max_attempts() {
            if let Err(e) = self
                .with_connection(tv_id, |conn| {
                    Box::pin(async move { conn.screen_off().await })
                })
                .await
            {
                debug!(tv_id, attempt, error = %e, "screen_off failed; giving up");
                return Err(e);
            }
            // Poll-only backends never push to `power_state_tx`, so the
            // passive await below would always time out and spuriously
            // warn "panel did not blank". Refresh the cached state with
            // a live query first (which also fires the watch) so the
            // await sees the fresh value. Push backends (LG/mock) skip
            // this — their subscription already updates the cache.
            if !has_push {
                self.refresh_power_state(tv_id, retry_config.verify_delay)
                    .await;
            }
            match self
                .await_power_state(tv_id, retry_config.verify_delay, |s| {
                    s == PowerState::ScreenOff
                })
                .await
            {
                AwaitOutcome::Matched(_) => return Ok(()),
                AwaitOutcome::TimedOut(last_seen) => {
                    debug!(
                        tv_id,
                        attempt,
                        cached_power_state = ?last_seen,
                        "screen_off did not reach screen_off state",
                    );
                }
            }
        }
        // No escalation — TV is reachable but refuses to blank.
        // Log and return Ok; subsequent screen_off attempts (from
        // user retry or repeated event) re-enter the loop with a
        // fresh snapshot.
        warn!(
            tv_id,
            "screen_off verify-retry exhausted; TV reachable but panel did not blank",
        );
        Ok(())
    }

    /// Override the per-TV desired-screen latch directly. The explicit
    /// `Tv1.Power.PowerOn` path uses it: an explicit power-on is an unambiguous
    /// "on and visible", so it sets the latch on (overriding any stale
    /// screen-off) *before* waking, so the push-driven reconcile keeps the
    /// freshly-woken panel on instead of re-blanking it.
    pub async fn set_desired_screen(&self, tv_id: &str, desired: Option<bool>) {
        if let Ok(tv_arc) = self.get_tv(tv_id).await {
            tv_arc.lock().await.desired_screen = desired;
        }
    }

    /// Issue `screen_on` and verify, with **no `power_on` escalation** — the
    /// bare counterpart of [`Self::screen_on`]. The push-driven
    /// [`Self::reconcile_screen`] uses this to re-assert the desired screen
    /// state on a TV that is already reachable, keeping the recursion-prone
    /// `power_on` out of the reconcile loop. The wake-from-deep-substate
    /// escalation stays on the direct `screen_on` path (PR #57's contract).
    /// `Ok(true)` if the panel verified into `Active`/`ScreenSaver`,
    /// `Ok(false)` if the verify budget was exhausted, `Err` if the command
    /// could not be sent.
    async fn apply_screen_on_raw(&self, tv_id: &str) -> Result<bool, TvManagerError> {
        let (retry_config, has_push) = self.verify_config(tv_id).await?;
        for attempt in 0..retry_config.effective_max_attempts() {
            self.with_connection(tv_id, |conn| {
                Box::pin(async move { conn.screen_on().await })
            })
            .await?;
            if !has_push {
                self.refresh_power_state(tv_id, retry_config.verify_delay)
                    .await;
            }
            if let AwaitOutcome::Matched(_) = self
                .await_power_state(tv_id, retry_config.verify_delay, |s| {
                    matches!(s, PowerState::Active | PowerState::ScreenSaver)
                })
                .await
            {
                return Ok(true);
            }
            debug!(
                tv_id,
                attempt, "apply_screen_on_raw: panel did not reach active"
            );
        }
        Ok(false)
    }

    /// Issue `screen_off` and verify, with **no `power_on` pre-flight** — the
    /// bare counterpart of [`Self::screen_off`], used by
    /// [`Self::reconcile_screen`]. Never powers the panel on to blank it (the
    /// momentary-flash that the direct `screen_off` accepts for the
    /// `on_startup = screen_off` case is the direct path's job, not the
    /// re-assert's).
    async fn apply_screen_off_raw(&self, tv_id: &str) -> Result<bool, TvManagerError> {
        let (retry_config, has_push) = self.verify_config(tv_id).await?;
        for attempt in 0..retry_config.effective_max_attempts() {
            self.with_connection(tv_id, |conn| {
                Box::pin(async move { conn.screen_off().await })
            })
            .await?;
            if !has_push {
                self.refresh_power_state(tv_id, retry_config.verify_delay)
                    .await;
            }
            if let AwaitOutcome::Matched(_) = self
                .await_power_state(tv_id, retry_config.verify_delay, |s| {
                    s == PowerState::ScreenOff
                })
                .await
            {
                return Ok(true);
            }
            debug!(tv_id, attempt, "apply_screen_off_raw: panel did not blank");
        }
        Ok(false)
    }

    /// Re-assert the TV's `desired_screen` toward the panel, **never**
    /// escalating to `power_on` (that stays on the direct `screen_on` /
    /// `screen_off` path, preserving PR #57's wake reliability). Called by the
    /// state-change forwarder on every power-state change, so a screen action
    /// that was deferred while the TV was reconnecting lands once it is
    /// reachable, and the latest intent re-asserts after a reconnect. No-ops
    /// when the cached state already matches the desire. Because reconcile
    /// holds no `power_on`, the `screen→power_on→state-push→reconcile` loop is
    /// impossible by construction.
    pub async fn reconcile_screen(&self, tv_id: &str) {
        // Never re-assert while a lifecycle wake is in flight: a stale push
        // could otherwise drive a bare apply that opens a second connection,
        // racing the in-flight `power_on` (the wake-race the direct
        // `screen_on` / `screen_off` defer prevents). And act only on automated
        // TVs, like every direct dispatch path. A deferred intent still lands —
        // the reconnect-complete `ConnectionChanged(true)` spawns a fresh
        // reconcile once `reconnecting` has cleared.
        if self.is_reconnecting(tv_id).await || !self.is_tv_automated(tv_id).await {
            return;
        }
        let (desired, observed) = {
            let Ok(tv_arc) = self.get_tv(tv_id).await else {
                return;
            };
            let tv = tv_arc.lock().await;
            (tv.desired_screen, tv.last_known_power_state)
        };
        let Some(want_on) = desired else {
            return;
        };
        // The panel is "lit" only in Active / ScreenSaver; every other state
        // (ScreenOff, ActiveStandby, Standby, …) is dark. Reconcile acts only
        // when the panel's lit-ness disagrees with the desire — so a desired-off
        // TV sitting in ActiveStandby (panel dark, e.g. #57's wake-refusal
        // graceful fallback) is already satisfied and we send nothing.
        let panel_lit = matches!(observed, PowerState::Active | PowerState::ScreenSaver);
        if panel_lit == want_on {
            return;
        }
        info!(
            tv_id,
            desired_on = want_on,
            observed = ?observed,
            "reconcile: re-asserting desired screen state",
        );
        // Re-check the intent under the lock right before applying: a concurrent
        // direct `screen_on` / `screen_off` may have flipped `desired_screen`
        // between the read above and now. The newer intent owns the outcome (its
        // own dispatch, or the next push-driven reconcile), so bail if it moved.
        {
            let Ok(tv_arc) = self.get_tv(tv_id).await else {
                return;
            };
            if tv_arc.lock().await.desired_screen != Some(want_on) {
                return;
            }
        }
        let result = if want_on {
            self.apply_screen_on_raw(tv_id).await
        } else {
            self.apply_screen_off_raw(tv_id).await
        };
        match result {
            Ok(true) => {}
            // Budget exhausted: the TV is reachable but refused to reach the
            // target. The forwarder only re-spawns reconcile on an actual
            // power-state change, so a TV stuck refusing won't thrash the verify
            // loop — but surface it for diagnosis.
            Ok(false) => debug!(
                tv_id,
                desired_on = want_on,
                "reconcile_screen: panel did not reach target (verify budget exhausted)",
            ),
            Err(e) => debug!(
                tv_id,
                desired_on = want_on,
                error = %e,
                "reconcile_screen: bare apply failed (TV unreachable?)",
            ),
        }
    }

    /// Read a TV's backend verify-retry config and push/poll discriminator
    /// under a brief lock. Shared by the bare `apply_screen_*_raw` paths.
    async fn verify_config(
        &self,
        tv_id: &str,
    ) -> Result<(tisvlay_core::RetryConfig, bool), TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;
        let tv = tv_arc.lock().await;
        let registered = self.registry.get(&tv.config.vendor).ok_or_else(|| {
            TvManagerError::TvNotFound(format!("vendor {:?} not registered", tv.config.vendor))
        })?;
        Ok((
            registered.backend.verify_retry_config(),
            tv.has_push_subscription,
        ))
    }

    // -------------------------------------------------------------------------
    // Volume Control
    // -------------------------------------------------------------------------

    /// Set volume.
    pub async fn set_volume(&self, tv_id: &str, volume: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_volume(volume).await })
        })
        .await?;
        Ok(())
    }

    /// Set mute.
    pub async fn set_mute(&self, tv_id: &str, mute: bool) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_mute(mute).await })
        })
        .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Input Control
    // -------------------------------------------------------------------------

    /// Switch input.
    pub async fn switch_input(&self, tv_id: &str, input_id: &str) -> Result<(), TvManagerError> {
        let input = input_id.to_string();
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.switch_input(&input).await })
        })
        .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Backlight Control
    // -------------------------------------------------------------------------

    /// Set backlight.
    pub async fn set_backlight(&self, tv_id: &str, backlight: u8) -> Result<(), TvManagerError> {
        self.with_connection(tv_id, |conn| {
            Box::pin(async move { conn.set_backlight(backlight).await })
        })
        .await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // TV Management
    // -------------------------------------------------------------------------

    /// Pair a new TV — async-spawn shape.
    ///
    /// Validates the vendor synchronously, reserves a fresh UUID, and
    /// spawns the pairing handshake into a background task. Returns
    /// the reserved tv_id immediately; clients drain
    /// `Manager1.PairingProgress(tv_id, …)` filtered on the returned
    /// tv_id to a terminal phase (`complete` or `failed`) to observe
    /// the outcome.
    ///
    /// On successful handshake the spawned task persists the config,
    /// installs the TV in the managed set, subscribes to backend
    /// state pushes, and registers the per-TV `Tv1.*` D-Bus
    /// interfaces — at which point the TV becomes visible via
    /// `ObjectManager.GetManagedObjects` + `InterfacesAdded`. On
    /// failed handshake the task does nothing further (PairingProgress
    /// already emitted `failed`); no TV is added.
    ///
    /// The `connection` is cloned into the spawned task so the task
    /// can register interfaces without going back through the D-Bus
    /// handler. `zbus::Connection` is `Clone` (Arc internally), so
    /// this is cheap.
    pub async fn pair_tv(
        self: &Arc<Self>,
        name: &str,
        vendor: &str,
        vendor_config: toml::Value,
        managed: bool,
        connection: zbus::Connection,
    ) -> Result<String, TvManagerError> {
        info!(name = %name, vendor = %vendor, "Pairing TV");

        // Synchronous validation: reject unknown vendor before
        // spawning anything.
        if self.registry.get(vendor).is_none() {
            return Err(TvManagerError::ConfigError(format!(
                "Unknown vendor: {}",
                vendor
            )));
        }

        // Generate a fresh UUID v4 upfront so PairingProgress signals
        // can carry a stable identifier and the client knows its
        // tv_id before any signal arrives. Decoupled from `name` so
        // renames don't churn cross-references.
        let uuid = uuid::Uuid::new_v4();
        let tv_id = uuid.to_string();

        // Spawn the handshake. The task owns clones of the
        // arguments; reports outcome via PairingProgress signals
        // (`run_pairing_handshake`'s internal progress task does the
        // emit). The RPC return doesn't wait for it.
        let self_arc = Arc::clone(self);
        let task_tv_id = tv_id.clone();
        let name_owned = name.to_string();
        let vendor_owned = vendor.to_string();
        tokio::spawn(async move {
            self_arc
                .run_pair_task(
                    connection,
                    uuid,
                    task_tv_id,
                    name_owned,
                    vendor_owned,
                    vendor_config,
                    managed,
                )
                .await;
        });

        Ok(tv_id)
    }

    /// Handshake + post-handshake registration for a new TV. Spawned
    /// by [`Self::pair_tv`]; runs the vendor pairing flow, persists
    /// the resulting config, installs the TV in the managed set, and
    /// registers per-TV `Tv1.*` D-Bus interfaces. Emits a terminal
    /// `PairingProgress` phase at the end — `complete` only if every
    /// step succeeded, `failed` otherwise. Clients observing
    /// `complete` can immediately call into the new Tv1.* interfaces
    /// without racing the registration.
    #[allow(clippy::too_many_arguments)]
    async fn run_pair_task(
        self: Arc<Self>,
        connection: zbus::Connection,
        uuid: uuid::Uuid,
        tv_id: String,
        name: String,
        vendor: String,
        vendor_config: toml::Value,
        managed: bool,
    ) {
        let result = self
            .try_run_pair_task(
                &connection,
                uuid,
                &tv_id,
                &name,
                &vendor,
                vendor_config,
                managed,
            )
            .await;

        // Single terminal-emit point. `run_pairing_handshake` no
        // longer emits terminal phases — they live here so they
        // never fire before the per-TV interfaces are registered
        // (the success case) or before the failure reason is
        // composed from any post-handshake step (the failure case).
        let (phase, message) = match &result {
            Ok(()) => ("complete", "Pairing complete".to_string()),
            Err(e) => ("failed", e.to_string()),
        };
        self.emit_state_change(StateChangeEvent::PairingProgress {
            tv_id: tv_id.clone(),
            phase: phase.to_string(),
            message,
        });

        match result {
            Ok(()) => info!(tv_id = %tv_id, "TV paired successfully"),
            Err(e) => warn!(tv_id = %tv_id, error = %e, "pair_tv task failed"),
        }
    }

    /// Inner body of [`Self::run_pair_task`]. Bubbles up any failure
    /// (handshake, post-pair connect, register-interfaces) as a
    /// `TvManagerError` so the outer task can emit a single
    /// `failed` PairingProgress with the composed reason.
    #[allow(clippy::too_many_arguments)]
    async fn try_run_pair_task(
        self: &Arc<Self>,
        connection: &zbus::Connection,
        uuid: uuid::Uuid,
        tv_id: &str,
        name: &str,
        vendor: &str,
        vendor_config: toml::Value,
        managed: bool,
    ) -> Result<(), TvManagerError> {
        // Handshake — emits awaiting_tv/awaiting_input along the way;
        // terminal is held back for the outer task.
        let final_config = self
            .run_pairing_handshake(tv_id, vendor, &vendor_config)
            .await?;

        // Build the persisted config from the handshake output.
        let mut tv_config = TvConfig::with_uuid(uuid, name, vendor, managed);
        tv_config
            .vendor_config
            .insert(vendor.to_string(), final_config);

        // Open the post-pair connection.
        let conn = self.registry.connect_tv(&tv_config).await.map_err(|e| {
            TvManagerError::ConnectionFailed {
                tv_id: tv_id.to_string(),
                source: e,
            }
        })?;

        // Install the TV in the managed set and start state
        // subscription. Build the `ManagedTv` and subscribe to
        // state pushes **before** taking the global `tvs.write()`
        // lock — `subscribe_state().await` can take hundreds of ms
        // against a real backend (LG: WebSocket round-trip), and
        // holding the write lock that long would gate every other
        // TV-map reader/writer.
        let system_profile = {
            let system = self.system_section.read().await;
            system.resolve_profile_by_uuid(tv_config.profile.system)
        };
        let mut managed_tv = ManagedTv::new(tv_config.clone(), system_profile);
        managed_tv.connection = Some(conn);
        self.refresh_subscription(tv_id, &mut managed_tv).await;
        {
            let mut tvs = self.tvs.write().await;
            tvs.insert(
                tv_id.to_string(),
                (tv_config.managed, Arc::new(Mutex::new(managed_tv))),
            );
        }

        // Persist. Disk-failure is logged but not fatal — in-memory
        // state is consistent and a future ReloadConfig will pick it
        // up. Don't fail the whole pair on a disk hiccup.
        if let Err(e) = self.save_tv_config(&tv_config).await {
            warn!(
                tv_id = %tv_id,
                error = %e,
                "pair_tv: failed to persist config; in-memory state is still valid",
            );
        }

        // Register per-TV D-Bus interfaces. Failure here means the
        // TV is in the managed set but invisible via
        // `ObjectManager.GetManagedObjects` — that's a hard failure
        // from the client's perspective. Roll the insert and the
        // disk write back so the terminal `failed` PairingProgress
        // truly means "no TV was added" (no orphan invisible TV
        // surviving across daemon restart).
        let state = self.get_current_state(tv_id).await;
        let path = tv_path(tv_id);
        let _ = unregister_tv_interfaces(connection, &path).await;
        if let Err(register_err) = register_tv_interfaces(
            connection,
            &path,
            tv_id.to_string(),
            Arc::clone(self),
            state.as_ref(),
        )
        .await
        {
            warn!(
                tv_id = %tv_id,
                error = %register_err,
                "pair_tv: register_tv_interfaces failed; rolling back in-memory + on-disk state",
            );
            {
                let mut tvs = self.tvs.write().await;
                tvs.shift_remove(tv_id);
            }
            if let Err(disk_err) = self.delete_tv_config(tv_id).await {
                warn!(
                    tv_id = %tv_id,
                    error = %disk_err,
                    "pair_tv rollback: failed to delete config from disk; disk state may persist until daemon restart",
                );
            }
            return Err(TvManagerError::ConfigError(format!(
                "Failed to register D-Bus interfaces (rolled back): {register_err}"
            )));
        }

        Ok(())
    }

    /// Run a pairing handshake against `vendor` with the given
    /// `vendor_config`, return the credentialed inner vendor_config.
    ///
    /// Shared by [`Self::pair_tv`] (new-TV path: caller generated a
    /// fresh UUID upfront) and [`Self::re_pair_tv`] (existing-TV path:
    /// caller passes the TV's existing tv_id and vendor_config). The
    /// `tv_id` is what subsequent `Manager1.PairingProgress` signals
    /// carry — clients watching progress can correlate to a known TV
    /// id before any persistent state lands.
    ///
    /// The pairing-state watcher is bounded by the session's lifetime
    /// (when `wait_for_completion` returns and the session drops, the
    /// watch channel closes and the task exits). On terminal state
    /// the task also exits the loop; the `abort()` after
    /// `wait_for_completion` is a belt-and-suspenders cleanup for the
    /// non-terminal-but-stuck case.
    async fn run_pairing_handshake(
        &self,
        tv_id: &str,
        vendor: &str,
        vendor_config: &toml::Value,
    ) -> Result<toml::Value, TvManagerError> {
        let mut session = self
            .registry
            .start_pairing(vendor, vendor_config)
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        // Register the session's command sender in `pairing_sessions`
        // so `Manager1.CancelPair(tv_id)` can reach the in-flight
        // session via `cancel_pair`. The RAII `PairingSessionGuard`
        // below removes the entry on every exit path — Ok return,
        // Err return, panic-unwind, or future-dropped mid-handshake.
        self.pairing_sessions
            .lock()
            .insert(tv_id.to_string(), session.command_sender());
        let _session_guard = PairingSessionGuard {
            pairing_sessions: &self.pairing_sessions,
            tv_id: tv_id.to_string(),
        };

        // Spawn a watcher that forwards non-terminal `PairingState`
        // changes as `Manager1.PairingProgress` events. Terminal
        // phases (`complete` / `failed`) are **not** emitted here —
        // they are the caller's responsibility (`run_pair_task` /
        // `run_re_pair_task`) so that:
        //
        // - `complete` only fires after the caller's post-handshake
        //   setup (connect, install, register interfaces) finishes.
        //   Clients seeing `complete` can immediately call into the
        //   per-TV `Tv1.*` interfaces without racing the registration.
        // - `failed` from a post-handshake setup failure (connect,
        //   registration, etc.) is also surfaced — the watcher can't
        //   see those.
        //
        // The watcher still observes the terminal state internally
        // (to know when to exit the loop), it just doesn't emit it.
        let mut state_rx = session.subscribe();
        let progress_tx = self.state_change_tx.clone();
        let progress_tv_id = tv_id.to_string();
        let progress_task = tokio::spawn(async move {
            // Initial state is *usually* `AwaitingTvPrompt`, but
            // there's a race against fast backends (mock with a
            // pre-seeded reject/accept outcome) that can flip the
            // state to terminal before this task reads `.borrow()`.
            // Suppress the emit if so — the outer task owns terminal
            // emission. Without this guard, a CI-fast race produces
            // a different `failed` message wire format than the
            // slow-path (the inner task emits the bare backend
            // reason; the outer task wraps it in
            // `TvManagerError::PairingFailed`'s `Pairing failed: …`
            // Display), which the GUI / CLI then surface
            // differently — flaky snapshots, confused users.
            let initial = state_rx.borrow().clone();
            if !initial.is_terminal() {
                let _ = progress_tx.send(StateChangeEvent::PairingProgress {
                    tv_id: progress_tv_id.clone(),
                    phase: initial.to_dbus_state().to_string(),
                    message: initial.message().to_string(),
                });
            }
            while state_rx.changed().await.is_ok() {
                let s = state_rx.borrow().clone();
                if s.is_terminal() {
                    break;
                }
                let _ = progress_tx.send(StateChangeEvent::PairingProgress {
                    tv_id: progress_tv_id.clone(),
                    phase: s.to_dbus_state().to_string(),
                    message: s.message().to_string(),
                });
            }
        });

        let result = session.wait_for_completion().await;
        progress_task.abort();

        // _session_guard drops here, removing the entry from
        // `pairing_sessions`.
        let credentials = result.map_err(|e| TvManagerError::PairingFailed(Box::new(e)))?;

        self.registry
            .build_config_from_pairing(vendor, vendor_config, &credentials)
            .map_err(|e| TvManagerError::PairingFailed(Box::new(e)))
    }

    /// Re-pair an existing TV: run the vendor backend's pairing
    /// handshake against the TV's existing `vendor_config`, persist
    /// the refreshed credentials in place, and reconnect. Used by
    /// `Tv1.RePair()` to recover from expired credentials (e.g., LG
    /// WebOS client-key revocation) without forcing the user to
    /// `Tv1.Remove` + `Manager1.AddTv` from scratch — top-level
    /// fields (`name`, `profile`, `managed`, `primary_input`) stay
    /// intact.
    ///
    /// # Lifecycle
    ///
    /// Sync part — the caller's `.await` returns immediately after
    /// these steps; failures here surface as RPC `Err`:
    ///
    /// 1. `get_tv(tv_id)` — `Err(TvNotFound)` on miss.
    /// 2. Lock the per-TV `Mutex<ManagedTv>`, check
    ///    `pairing_in_flight` — `Err(PairingFailed("already
    ///    pairing"))` if a re-pair is already in flight.
    /// 3. Claim the slot, capture `was_connected`, close existing
    ///    connection, snapshot `(vendor, vendor_config)`.
    /// 4. Release the lock.
    /// 5. If `was_connected`: emit `ConnectionChanged{false}` so
    ///    clients reflect the intended disconnect during the
    ///    handshake window.
    /// 6. Spawn `run_re_pair_task` and return `Ok(())`.
    ///
    /// Async part (`run_re_pair_task`, signal-driven; no caller is
    /// awaiting this): handshake → on Ok, update `vendor_config` +
    /// persist + emit `TvVendorConfigChanged` + reconnect with new
    /// credentials. On Err, attempt best-effort reconnect with the
    /// unchanged old `vendor_config`. Terminal `PairingProgress` is
    /// emitted at the end:
    ///
    /// | Path                                       | RPC return | Terminal phase | Connected |
    /// |--------------------------------------------|------------|----------------|-----------|
    /// | Success                                    | `Ok(())`   | `complete`     | false → true (via reconnect) |
    /// | Handshake failed, old credentials work     | `Ok(())`   | `failed`       | false → true (best-effort reconnect) |
    /// | Handshake failed, old credentials dead     | `Ok(())`   | `failed`       | false → stays false |
    /// | Sync validation error (steps 1–2 above)    | `Err(...)` | (no signal)    | unchanged |
    ///
    /// The best-effort reconnect outcome is **not** surfaced through
    /// `PairingProgress` — that's about TV reachability, conceptually
    /// orthogonal to whether re-pair succeeded. Clients that care
    /// about reachability watch `Tv1.Connected`.
    ///
    /// # Behavior note (LG)
    ///
    /// LG's `start_pairing` always treats the call as a fresh pair
    /// regardless of the existing `client_key` (see
    /// `crates/tisvlay-lg/src/backend.rs::start_pairing` —
    /// `WebOsClient::connect(&ip, None)`). So every re-pair triggers
    /// a fresh permission prompt on the TV.
    pub async fn re_pair_tv(self: &Arc<Self>, tv_id: &str) -> Result<(), TvManagerError> {
        info!(tv_id = %tv_id, "Re-pairing TV");

        let tv_arc = self.get_tv(tv_id).await?;

        // Validate state + claim pairing_in_flight slot under per-TV
        // Mutex. Capture the vendor + existing vendor_config under
        // the same lock so the handshake works against a consistent
        // snapshot even if some other path mutates `tv.config`.
        // `was_connected` lets us decide whether the error path
        // should attempt a best-effort reconnect with the old
        // (still-on-disk) credentials.
        let (vendor, vendor_config, was_connected) = {
            let mut tv = tv_arc.lock().await;
            if tv.pairing_in_flight {
                return Err(TvManagerError::PairingFailed(Box::new(
                    tisvlay_core::Error::backend(&tv.config.vendor, "already pairing"),
                )));
            }
            tv.pairing_in_flight = true;
            // Close existing connection so the handshake starts fresh.
            // Failing to close cleanly isn't fatal — the connection
            // is being replaced regardless.
            let was_connected = tv.connection.is_some();
            if let Some(mut conn) = tv.connection.take() {
                let _ = conn.close().await;
            }
            let vendor = tv.config.vendor.clone();
            let vendor_config = tv
                .config
                .vendor_config
                .get(&vendor)
                .cloned()
                .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
            (vendor, vendor_config, was_connected)
        };

        // Tell clients the TV is disconnected for the duration of
        // the handshake. Without this the D-Bus `Connected` property
        // cache + GUI Connected indicator stay stale-true for the
        // 10-30s handshake window. The matching `true` event is
        // emitted by `self.reconnect()` on success (inside the task).
        if was_connected {
            self.emit_connection_changed(tv_id, false);
        }

        // Spawn the handshake. The task owns its arguments and
        // reports outcomes via PairingProgress signals; this RPC
        // returns Ok as soon as the spawn is in flight.
        let self_arc = Arc::clone(self);
        let task_tv_id = tv_id.to_string();
        let task_tv_arc = Arc::clone(&tv_arc);
        tokio::spawn(async move {
            self_arc
                .run_re_pair_task(
                    task_tv_id,
                    vendor,
                    vendor_config,
                    was_connected,
                    task_tv_arc,
                )
                .await;
        });

        Ok(())
    }

    /// Handshake + post-handshake reconnect for `re_pair_tv`. Spawned
    /// by [`Self::re_pair_tv`]; runs the vendor pairing handshake
    /// against the existing `vendor_config`, persists refreshed
    /// credentials on success, attempts a reconnect to bring the TV
    /// back online. On failure, attempts a best-effort reconnect
    /// with the unchanged old `vendor_config` so a cancelled re-pair
    /// doesn't tear down a working session.
    ///
    /// Emits a terminal `PairingProgress` phase at the end:
    /// `complete` if the handshake succeeded (regardless of the
    /// post-handshake reconnect outcome — re-pair is about
    /// credentials, not reachability; clients observe reachability
    /// via `Tv1.Connected`), `failed` if the handshake itself
    /// failed.
    async fn run_re_pair_task(
        self: Arc<Self>,
        tv_id: String,
        vendor: String,
        vendor_config: toml::Value,
        was_connected: bool,
        tv_arc: Arc<Mutex<ManagedTv>>,
    ) {
        let handshake_result = self
            .run_pairing_handshake(&tv_id, &vendor, &vendor_config)
            .await;

        // Clear the flag + apply result. Done in one critical
        // section so the flag is always cleaned up even if persist
        // fails below.
        let new_tv_config = {
            let mut tv = tv_arc.lock().await;
            tv.pairing_in_flight = false;
            match handshake_result {
                Ok(new_inner_config) => {
                    tv.config
                        .vendor_config
                        .insert(vendor.clone(), new_inner_config);
                    tv.config.clone()
                }
                Err(e) => {
                    // Handshake failed (user cancelled, TV rejected,
                    // timeout). If we'd been connected, attempt a
                    // best-effort reconnect with the unchanged
                    // `vendor_config` so a stray Cancel doesn't tear
                    // down a working session. If the old credentials
                    // were genuinely expired the reconnect fails too;
                    // the user sees the truth via `Tv1.Connected`.
                    drop(tv);
                    if was_connected && let Err(reconn_err) = self.reconnect(&tv_id).await {
                        warn!(
                            tv_id = %tv_id,
                            error = %reconn_err,
                            "Re-pair: best-effort reconnect after handshake error failed",
                        );
                    }
                    warn!(tv_id = %tv_id, error = %e, "Re-pair handshake failed");
                    self.emit_state_change(StateChangeEvent::PairingProgress {
                        tv_id: tv_id.clone(),
                        phase: "failed".to_string(),
                        message: e.to_string(),
                    });
                    return;
                }
            }
        };

        // Persist the refreshed credentials so a daemon restart picks
        // them up automatically. Disk I/O outside the per-TV Mutex —
        // `save_tv_config` serializes via its own `disk_write_lock`.
        // Persist-failure is logged but not fatal: in-memory state
        // is consistent, and the next ReloadConfig (or daemon
        // restart with the new disk write that succeeded later) will
        // sync it.
        if let Err(e) = self.save_tv_config(&new_tv_config).await {
            warn!(
                tv_id = %tv_id,
                error = %e,
                "Re-pair: failed to persist refreshed config; in-memory state still valid",
            );
        }

        // Tell the state forwarder that vendor_config changed so the
        // per-TV `Tv1.*` cached fields get refreshed (same path
        // `Tv1.UpdateConfig` uses for vendor-side edits).
        self.emit_state_change(StateChangeEvent::TvVendorConfigChanged {
            tv_id: tv_id.clone(),
        });

        // Reconnect with the new credentials. Reuses the standard
        // `reconnect` flow which handles WoL + connection-retry +
        // subscribe-state under the per-TV Mutex. Failure here
        // doesn't roll back the re-pair: credentials were refreshed,
        // but the TV is unreachable for some other reason. Clients
        // see this via `Tv1.Connected = false`. The terminal
        // PairingProgress below is still `complete` because the
        // re-pair operation itself succeeded.
        if let Err(e) = self.reconnect(&tv_id).await {
            warn!(
                tv_id = %tv_id,
                error = %e,
                "Re-pair: reconnect with new credentials failed; subsequent commands will retry"
            );
        }

        info!(tv_id = %tv_id, "Re-pair complete");
        self.emit_state_change(StateChangeEvent::PairingProgress {
            tv_id: tv_id.clone(),
            phase: "complete".to_string(),
            message: "Re-pair complete".to_string(),
        });
    }

    /// Cancel an in-flight pairing handshake for the given TV.
    ///
    /// Looks up the session's command sender in `pairing_sessions`
    /// and sends `PairingCommand::Cancel`. The session's
    /// `wait_for_completion` will then resolve with
    /// `PairingError::cancelled`, `run_pairing_handshake` will
    /// return `Err(PairingFailed)`, and the outer
    /// `run_pair_task` / `run_re_pair_task` will emit a terminal
    /// `failed` `PairingProgress` carrying the cancellation reason.
    ///
    /// Returns `Err(NotPairing)` if no in-flight session exists for
    /// the tv_id (already completed, never started, wrong id).
    /// Strict variant matching BlueZ's `Adapter.RemoveDevice`
    /// on-unknown-device convention.
    ///
    /// # Cancel during the spawn gap
    ///
    /// `pair_tv` / `re_pair_tv` return the reserved tv_id (or
    /// `Ok(())`) before the spawned task has reached
    /// `run_pairing_handshake`'s `pairing_sessions.insert(...)`. A
    /// `CancelPair(tv_id)` call landing in that microsecond window
    /// returns `NotPairing` because the map entry doesn't exist
    /// yet. In practice the GUI's Cancel button is only enabled
    /// after the first `PairingProgress` signal arrives, which
    /// only fires after the entry exists — the gap is invisible to
    /// human-driven cancels. Closing it would require a pre-emit
    /// insert on the caller's stack; not worth the complexity for
    /// the rare machine-driven race.
    pub async fn cancel_pair(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let sender = self.pairing_sessions.lock().get(tv_id).cloned();
        let Some(sender) = sender else {
            return Err(TvManagerError::NotPairing(tv_id.to_string()));
        };
        // Send Cancel via the channel. If the receiver is gone (race
        // where the session finished between our `get` and the
        // `send`), we treat it as "not pairing" — same observable
        // result the client would see if they called CancelPair a
        // moment later. tisvlay_core::PairingCommand is re-exported
        // from the crate root.
        //
        // Also evict the stale entry on send failure so a permanently
        // dead receiver doesn't leak in the map. `PairingSessionGuard`
        // normally handles cleanup, but if the handshake future was
        // dropped *between* the guard's insert and its drop (e.g.,
        // task aborted mid-await), the entry can survive with a dead
        // sender until a future cancel_pair stumbles on it.
        if sender
            .send(tisvlay_core::PairingCommand::Cancel)
            .await
            .is_err()
        {
            self.pairing_sessions.lock().remove(tv_id);
            return Err(TvManagerError::NotPairing(tv_id.to_string()));
        }
        Ok(())
    }

    /// Remove a TV.
    ///
    /// # Drain semantics for in-flight `Tv1.*` calls
    ///
    /// The per-TV `Mutex<ManagedTv>` (the `Arc<Mutex<ManagedTv>>` stored in
    /// the `tvs` map) is the de-facto drain layer between this method
    /// and concurrent `Tv1.*` D-Bus method calls against the same TV.
    /// zbus 5.x has no drain primitive (`ObjectServer::remove` mutates
    /// its routing map and returns; in-flight handlers complete against
    /// a previously-cloned `Arc<RwLock<dyn Interface>>`), so the
    /// observable correctness of "remove during in-flight call" rests
    /// on this Mutex pattern.
    ///
    /// Three sub-cases for an in-flight `Tv1.SetVolume`-like call
    /// dispatched while `remove_tv` runs:
    ///
    /// - **(a) Call hasn't reached `get_tv` yet.** The outer write
    ///   lock on `tvs` removes the entry before the call's lookup; the
    ///   call's `get_tv` returns `Err(TvNotFound)`. Client sees
    ///   `zbus::fdo::Error::Failed("TV not found: ...")`.
    ///
    /// - **(b) Call holds the `Arc<Mutex<ManagedTv>>` (post-`get_tv`)
    ///   but is awaiting the inner Mutex.** Removal acquires the
    ///   Mutex first (after `shift_remove` releases the outer write
    ///   lock), closes the connection inside the lock, drops it. The
    ///   in-flight call then acquires the Mutex, finds
    ///   `connection = None`, returns `Err(NotConnected)`. Client sees
    ///   `zbus::fdo::Error::Failed("Not connected to TV: ...")`.
    ///
    /// - **(c) Call holds the Mutex during SSAP work.** Removal
    ///   *blocks* waiting on the Mutex until the in-flight write
    ///   completes. The in-flight call returns `Ok` against the still-
    ///   open connection. Removal then proceeds, closes the connection,
    ///   and finishes. Client sees the write succeed; the subsequent
    ///   removal is invisible to that caller.
    ///
    /// Every observable outcome is coherent — no stale-Ok-with-no-effect
    /// scenario exists. The Mutex-held-across-await pattern (in
    /// `with_connection` and similar helpers) is what makes case (c)
    /// safe; a future refactor that drops it (e.g., releases the
    /// Mutex before the SSAP send) would re-introduce the race.
    ///
    /// `apply_reload_diff`'s removed-half loop uses the same
    /// `tv_manager.remove_tv` path indirectly (via `tv_manager::reload_config`),
    /// so the same drain semantics apply to direct config-file
    /// edits that drop a TV.
    pub async fn remove_tv(&self, tv_id: &str) -> Result<(), TvManagerError> {
        info!(tv_id = %tv_id, "Removing TV");

        // Remove from managed TVs (outer write lock -- modifying the HashMap)
        let removed_arc = {
            let mut tvs = self.tvs.write().await;
            tvs.shift_remove(tv_id).map(|(_, arc)| arc)
        };

        match removed_arc {
            Some(tv_arc) => {
                // Close connection if any (per-TV lock)
                let mut tv = tv_arc.lock().await;
                if let Some(mut conn) = tv.connection.take() {
                    let _ = conn.close().await;
                }
            }
            None => {
                return Err(TvManagerError::TvNotFound(tv_id.to_string()));
            }
        }

        // Remove from config file
        self.delete_tv_config(tv_id).await?;

        info!(tv_id = %tv_id, "TV removed successfully");
        Ok(())
    }

    /// Save a TV config to the configuration file.
    ///
    /// Serializes disk I/O via `disk_write_lock` to prevent concurrent load-modify-save
    /// races. Uses `spawn_blocking` to avoid blocking the async executor.
    async fn save_tv_config(&self, tv_config: &TvConfig) -> Result<(), TvManagerError> {
        let config_path = self.config_path.clone();
        let tv_config = tv_config.clone();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            config.upsert_tv(tv_config);
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))?
    }

    /// Delete a TV from the configuration file.
    ///
    /// Serializes disk I/O via `disk_write_lock` to prevent concurrent load-modify-save
    /// races. Uses `spawn_blocking` to avoid blocking the async executor.
    async fn delete_tv_config(&self, tv_id: &str) -> Result<(), TvManagerError> {
        let config_path = self.config_path.clone();
        let tv_id = tv_id.to_string();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            config.remove_tv(&tv_id);
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))?
    }

    /// Set a single top-level TV option and persist to disk.
    ///
    /// Crate-internal helper: the per-property `Tv1.{Name,Managed,
    /// SystemProfile,PrimaryInput}` writable handlers route through
    /// here for single-field flips. Multi-field atomic edits go via
    /// [`update_tv_config`](Self::update_tv_config) instead.
    ///
    /// Applies the change to the in-memory TV config first (which
    /// also validates the TV ID and key), then persists via
    /// `spawn_blocking` to avoid blocking the async executor on
    /// disk I/O.
    pub(crate) async fn set_tv_option(
        &self,
        tv_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), TvManagerError> {
        // Validate and apply to in-memory TV config first.
        // Validate system profile UUID exists in the catalog before applying.
        if key == "profile.system" {
            let parsed: uuid::Uuid = value.parse().map_err(|_| {
                TvManagerError::ConfigError(format!("Invalid system profile UUID: {value}"))
            })?;
            let system = self.system_section.read().await;
            if !system.profiles.contains_key(&parsed) {
                return Err(TvManagerError::ConfigError(format!(
                    "Unknown system profile: {value}"
                )));
            }
        }

        let tv_arc = self.get_tv(tv_id).await?;
        {
            let mut managed = tv_arc.lock().await;
            apply_tv_option(&mut managed.config, key, value)?;

            // Re-resolve the system profile when the assignment changes.
            if key == "profile.system" {
                let system = self.system_section.read().await;
                managed.system_profile =
                    system.resolve_profile_by_uuid(managed.config.profile.system);
            }
        } // per-TV lock released before I/O

        // Keep the outer managed flag in sync when this helper toggles `managed`.
        if key == "managed"
            && let Ok(managed_val) = value.parse::<bool>()
        {
            let mut tvs = self.tvs.write().await;
            if let Some((flag, _)) = tvs.get_mut(tv_id) {
                *flag = managed_val;
            }
        }

        // Persist to disk: load the full on-disk config, patch the TV entry, save.
        // Serialize via disk_write_lock to prevent concurrent load-modify-save races.
        let config_path = self.config_path.clone();
        let tv_id_owned = tv_id.to_string();
        let key_owned = key.to_string();
        let value_owned = value.to_string();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            let tv = config
                .find_tv_mut(&tv_id_owned)
                .ok_or_else(|| TvManagerError::TvNotFound(tv_id_owned.clone()))?;
            apply_tv_option(tv, &key_owned, &value_owned)?;
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))??;

        self.emit_state_change(StateChangeEvent::OptionChanged {
            tv_id: tv_id.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        });
        info!(tv_id = %tv_id, key = %key, value = %value, "TV option updated");
        Ok(())
    }

    /// Render the current TV config as a JSON object suitable for
    /// prefilling the GUI's edit pane.
    ///
    /// The payload mirrors the keys the schema-driven form expects:
    /// top-level `name` / `managed` / `system_profile` /
    /// `primary_input` keys (mapped from `tv.config.profile.system`
    /// for the system-profile key) plus every vendor-config field
    /// from the connector's `config_schema()` filtered to non-`Secret`
    /// visibilities. `Secret` fields (LG `client_key` etc.) are
    /// stripped server-side so they can never reach the GUI even
    /// if a future schema slip-up forgets to mark a field.
    pub async fn get_tv_config_json(&self, tv_id: &str) -> Result<String, TvManagerError> {
        let tv_arc = self.get_tv(tv_id).await?;
        let tv = tv_arc.lock().await;
        let config = &tv.config;

        let mut obj = serde_json::Map::new();
        obj.insert(
            "name".to_string(),
            serde_json::Value::String(config.name.clone()),
        );
        obj.insert(
            "managed".to_string(),
            serde_json::Value::Bool(config.managed),
        );
        obj.insert(
            "system_profile".to_string(),
            serde_json::Value::String(config.profile.system.to_string()),
        );
        if let Some(ref pi) = config.primary_input {
            obj.insert(
                "primary_input".to_string(),
                serde_json::Value::String(pi.clone()),
            );
        }

        let connector = self
            .registry
            .get_connector(&config.vendor)
            .map_err(|e| TvManagerError::ConfigError(format!("Unknown vendor: {}", e)))?;
        let schema = connector.config_schema();
        let vendor_table = config.get_raw_vendor_value().and_then(|v| v.as_table());

        for field in &schema {
            if field.visibility == FieldVisibility::Secret {
                continue;
            }
            if let Some(value) = vendor_table.and_then(|t| t.get(&field.name)) {
                obj.insert(field.name.clone(), crate::json_toml::toml_to_json(value));
            }
        }

        serde_json::to_string(&serde_json::Value::Object(obj))
            .map_err(|e| TvManagerError::ConfigError(format!("Serialize config: {}", e)))
    }

    /// Apply a JSON patch atomically to a TV's config.
    ///
    /// Accepted top-level keys: `name`, `managed`, `system_profile`,
    /// `primary_input` (mirrors the keys `set_tv_option` accepts;
    /// `system_profile` maps to the internal `profile.system` slot).
    /// All other keys are interpreted as vendor-config fields and
    /// must be `UserInput`-visibility entries in the connector's
    /// `config_schema()`. `Secret` and `Diagnostic` keys are
    /// rejected — defense in depth, the daemon never trusts a
    /// client to filter on visibility for it.
    ///
    /// Each vendor field is canonically validated server-side via
    /// `PairingField::validate` (same checks `AddTv` runs at pair
    /// time), so a field that passes here passes there. Validation
    /// errors map to `ConfigError::InvalidVendorConfig { field,
    /// reason }` so the GUI's existing inline-error routing handles
    /// them unchanged.
    ///
    /// Vendor patches are merged into the existing
    /// `vendor_config[vendor]` table — keys not in the patch keep
    /// their current values. After persisting, if any vendor field
    /// changed the connection is dropped and the daemon reconnects
    /// (covers IP changes; `mac` changes don't break the connection
    /// but the reconnect is harmless).
    pub async fn update_tv_config(
        self: &Arc<Self>,
        tv_id: &str,
        json_patch: &str,
    ) -> Result<(), TvManagerError> {
        const TOP_LEVEL_KEYS: &[&str] = &["name", "managed", "system_profile", "primary_input"];

        let patch: serde_json::Value = serde_json::from_str(json_patch)
            .map_err(|e| TvManagerError::ConfigError(format!("Invalid JSON patch: {}", e)))?;
        let patch_obj = patch.as_object().ok_or_else(|| {
            TvManagerError::ConfigError("patch must be a JSON object".to_string())
        })?;

        let tv_arc = self.get_tv(tv_id).await?;
        let vendor = {
            let tv = tv_arc.lock().await;
            tv.config.vendor.clone()
        };

        let connector = self
            .registry
            .get_connector(&vendor)
            .map_err(|e| TvManagerError::ConfigError(format!("Unknown vendor: {}", e)))?;
        let schema = connector.config_schema();

        // Sort patches into top-level + vendor buckets, validating
        // along the way so we reject a malformed patch before
        // touching disk or in-memory state.
        let mut top_level: Vec<(String, String)> = Vec::new();
        let mut vendor_patches: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

        for (key, value) in patch_obj {
            if TOP_LEVEL_KEYS.contains(&key.as_str()) {
                let str_value = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Null => String::new(),
                    _ => {
                        return Err(TvManagerError::ConfigError(format!(
                            "Invalid value type for top-level field '{}'",
                            key
                        )));
                    }
                };
                if key == "system_profile" {
                    let parsed: uuid::Uuid = str_value.parse().map_err(|_| {
                        TvManagerError::ConfigError(format!(
                            "Invalid system profile UUID: {str_value}"
                        ))
                    })?;
                    let system = self.system_section.read().await;
                    if !system.profiles.contains_key(&parsed) {
                        return Err(TvManagerError::ConfigError(format!(
                            "Unknown system profile: {str_value}"
                        )));
                    }
                }
                top_level.push((key.clone(), str_value));
                continue;
            }

            let field = schema.iter().find(|f| f.name == *key).ok_or_else(|| {
                TvManagerError::ConfigError(format!(
                    "Field '{}' is not in the {} config schema",
                    key, vendor
                ))
            })?;
            if field.visibility != FieldVisibility::UserInput {
                return Err(TvManagerError::ConfigError(format!(
                    "Field '{}' is not editable (visibility = {:?})",
                    key, field.visibility
                )));
            }
            let str_value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            field.validate(&str_value).map_err(|e| {
                TvManagerError::ConfigError(format!("Validation failed for field '{}': {}", key, e))
            })?;
            vendor_patches.insert(key.clone(), value.clone());
        }

        // Apply to in-memory config under the per-TV lock.
        let mut managed_changed_to: Option<bool> = None;
        let mut system_profile_changed = false;
        {
            let mut tv = tv_arc.lock().await;
            // Re-pair runs its handshake outside the per-TV lock for
            // the ~10-30s handshake duration; a concurrent UpdateConfig
            // that modifies vendor_config during that window would be
            // silently overwritten when `re_pair_tv` re-acquires the
            // lock and inserts its handshake-derived table. Reject
            // outright while pairing is in flight — the client retries
            // after re-pair completes.
            if tv.pairing_in_flight {
                // Re-routes to `DaemonError::OperationFailed` at the
                // D-Bus layer (see `dbus::mod::From<TvManagerError>`).
                return Err(TvManagerError::ConfigError(format!(
                    "TV {} is currently pairing; config edits are blocked until pairing completes",
                    tv_id
                )));
            }
            for (key, value) in &top_level {
                let internal_key = if key == "system_profile" {
                    "profile.system"
                } else {
                    key.as_str()
                };
                apply_tv_option(&mut tv.config, internal_key, value)?;
                if key == "system_profile" {
                    system_profile_changed = true;
                }
                if key == "managed"
                    && let Ok(b) = value.parse::<bool>()
                {
                    managed_changed_to = Some(b);
                }
            }
            for (key, value) in &vendor_patches {
                let toml_value =
                    crate::json_toml::json_to_toml(value).map_err(TvManagerError::ConfigError)?;
                let entry = tv
                    .config
                    .vendor_config
                    .entry(vendor.clone())
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
                if let toml::Value::Table(table) = entry {
                    table.insert(key.clone(), toml_value);
                }
            }
            if system_profile_changed {
                let system = self.system_section.read().await;
                tv.system_profile = system.resolve_profile_by_uuid(tv.config.profile.system);
            }
        }

        // Mirror managed flag onto the outer per-TV tuple so
        // automation gating sees the new value without a reload.
        if let Some(b) = managed_changed_to {
            let mut tvs = self.tvs.write().await;
            if let Some((flag, _)) = tvs.get_mut(tv_id) {
                *flag = b;
            }
        }

        // Persist to disk: load → patch the matching `[[tv]]` →
        // save. Same disk-write-lock + spawn_blocking pattern the
        // rest of `tv_manager` uses.
        let config_path = self.config_path.clone();
        let tv_id_owned = tv_id.to_string();
        let top_level_owned = top_level.clone();
        let vendor_patches_owned = vendor_patches.clone();
        let vendor_owned = vendor.clone();
        let _disk_lock = self.disk_write_lock.lock().await;
        tokio::task::spawn_blocking(move || -> Result<(), TvManagerError> {
            let mut config = SystemConfig::load_from(&config_path).map_err(|e| {
                TvManagerError::ConfigError(format!("Failed to load config: {}", e))
            })?;
            let tv = config
                .find_tv_mut(&tv_id_owned)
                .ok_or_else(|| TvManagerError::TvNotFound(tv_id_owned.clone()))?;
            for (key, value) in &top_level_owned {
                let internal_key = if key == "system_profile" {
                    "profile.system"
                } else {
                    key.as_str()
                };
                apply_tv_option(tv, internal_key, value)?;
            }
            for (key, value) in &vendor_patches_owned {
                let toml_value =
                    crate::json_toml::json_to_toml(value).map_err(TvManagerError::ConfigError)?;
                let entry = tv
                    .vendor_config
                    .entry(vendor_owned.clone())
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
                if let toml::Value::Table(table) = entry {
                    table.insert(key.clone(), toml_value);
                }
            }
            config
                .save_to(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to save config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))??;
        drop(_disk_lock);

        // Emit per-key OptionChanged events so `state_forwarder`
        // refreshes the cached fields on the per-TV `Tv1` interface
        // (managed / name / system_profile / primary_input). Without
        // these the in-memory daemon state is correct but D-Bus
        // property reads return the stale cached value until the
        // next config reload.
        for (key, value) in &top_level {
            let internal_key = if key == "system_profile" {
                "profile.system"
            } else {
                key.as_str()
            };
            self.emit_state_change(StateChangeEvent::OptionChanged {
                tv_id: tv_id.to_string(),
                key: internal_key.to_string(),
                value: value.clone(),
            });
        }

        // Vendor-field changes invalidate the live connection (IP
        // moved, port changed, etc.). Drop + reconnect; the next
        // command goes over the new transport. Also fire a
        // `TvVendorConfigChanged` event so the forwarder refreshes
        // per-TV cached properties that derive from vendor config —
        // `Tv1.VendorInfo` (label/value projection) and
        // `Tv1.Power.SupportsWake` (depends on MAC presence). The
        // forwarder is responsible for the property refresh; the
        // reconnect handles transport semantics independently.
        if !vendor_patches.is_empty() {
            self.emit_state_change(StateChangeEvent::TvVendorConfigChanged {
                tv_id: tv_id.to_string(),
            });
            if let Err(e) = self.reconnect(tv_id).await {
                warn!(
                    tv_id = %tv_id,
                    error = %e,
                    "Failed to reconnect after vendor config update; subsequent commands will retry"
                );
            }
        }

        info!(
            tv_id = %tv_id,
            top_level_changed = top_level.len(),
            vendor_changed = vendor_patches.len(),
            "TV config updated"
        );
        Ok(())
    }

    /// Reload configuration from disk.
    ///
    /// Uses `spawn_blocking` to avoid blocking the async executor on disk I/O,
    /// consistent with all other config-load sites in this file.
    ///
    /// Returns a [`ReloadDiff`] of TV ids that appeared or disappeared so
    /// callers (the dbus layer, the SIGHUP handler) can register or
    /// unregister the corresponding `Tv1.*` object paths.
    pub async fn reload_config(&self) -> Result<ReloadDiff, TvManagerError> {
        info!("Reloading configuration");

        let config_path = self.config_path.clone();
        let _disk_lock = self.disk_write_lock.lock().await;
        let new_config = tokio::task::spawn_blocking(move || {
            SystemConfig::load_from(&config_path)
                .map_err(|e| TvManagerError::ConfigError(format!("Failed to load config: {}", e)))
        })
        .await
        .map_err(|e| TvManagerError::ConfigError(format!("Task join error: {}", e)))??;

        let new_ids: std::collections::HashSet<_> =
            new_config.tv.iter().map(|t| t.uuid.to_string()).collect();

        // Stage 1: update the IndexMap under write lock (map mutations only — no I/O).
        // Collect Arcs for removed TVs and existing TVs that need config updates.
        // We do NOT close connections here; the write lock must not be held across
        // async WebSocket close operations that can block for hundreds of milliseconds.
        let mut to_update: Vec<(Arc<Mutex<ManagedTv>>, Option<TvConfig>)> = Vec::new();
        let mut diff = ReloadDiff::default();
        let tv_count;
        {
            let mut tvs = self.tvs.write().await;

            // Remove TVs that are no longer in config
            let old_ids: Vec<_> = tvs.keys().cloned().collect();
            for old_id in old_ids {
                if !new_ids.contains(&old_id) {
                    info!(tv_id = %old_id, "Removing TV (no longer in config)");
                    if let Some((_, arc)) = tvs.shift_remove(&old_id) {
                        to_update.push((arc, None)); // None = removed, close connection
                        diff.removed.push(old_id);
                    }
                }
            }

            // Add or update TVs from new config
            for tv_config in new_config.tv {
                let tv_id = tv_config.uuid.to_string();
                if let Some((managed_flag, tv_arc)) = tvs.get_mut(&tv_id) {
                    // Existing TV: update outer managed flag immediately under the write
                    // lock, then defer the rest of the config update to Stage 2.
                    *managed_flag = tv_config.managed;
                    to_update.push((tv_arc.clone(), Some(tv_config)));
                } else {
                    // New TV: add to map immediately (no connection to close).
                    info!(tv_id = %tv_id, "Adding new TV from config");
                    let system_profile = new_config
                        .system
                        .resolve_profile_by_uuid(tv_config.profile.system);
                    tvs.insert(
                        tv_id.clone(),
                        (
                            tv_config.managed,
                            Arc::new(Mutex::new(ManagedTv::new(tv_config, system_profile))),
                        ),
                    );
                    diff.added.push(tv_id);
                }
            }

            tv_count = tvs.len();
        } // write lock released

        // Stage 2: update configs and close connections without holding the outer lock.
        // Each per-TV lock is held only for the duration of its own operation.
        for (tv_arc, new_tv_config) in to_update {
            let mut tv = tv_arc.lock().await;
            match new_tv_config {
                None => {
                    // TV removed from config — close its connection.
                    if let Some(mut conn) = tv.connection.take() {
                        let _ = conn.close().await;
                    }
                }
                Some(tv_config) => {
                    // Existing TV — reconnect if connection params changed.
                    let connection_changed = tv.config.vendor != tv_config.vendor
                        || tv.config.vendor_config != tv_config.vendor_config;
                    if connection_changed {
                        info!(tv_id = %tv_config.uuid, "Updating TV config (will reconnect)");
                        if let Some(mut conn) = tv.connection.take() {
                            let _ = conn.close().await;
                        }
                    }
                    tv.config = tv_config;
                }
            }
        }

        info!(tv_count = tv_count, "Configuration reloaded");

        // Update system section (system profiles).
        //
        // Snapshot the old catalog under the write lock, compute the
        // three-set diff against `new_config.system.profiles`, then
        // swap. The diff lists are surfaced via `ReloadDiff` so the
        // dbus layer can emit per-profile signals (Added / Changed /
        // Removed) that match the wire contract of the equivalent
        // external mutation calls. Without this clients with cached
        // profile bodies stay stale until they hit a `Get*` call.
        {
            let mut system = self.system_section.write().await;
            let old_profiles = std::mem::take(&mut system.profiles);
            let new_profiles = &new_config.system.profiles;

            for (uuid, new_body) in new_profiles {
                let uuid_str = uuid.to_string();
                match old_profiles.get(uuid) {
                    Some(old_body) if old_body == new_body => { /* unchanged */ }
                    Some(_) => diff.system_profiles_changed.push(uuid_str),
                    None => diff.system_profiles_added.push(uuid_str),
                }
            }
            for uuid in old_profiles.keys() {
                if !new_profiles.contains_key(uuid) {
                    diff.system_profiles_removed.push(uuid.to_string());
                }
            }

            *system = new_config.system;
        }

        // Update user section (user profiles). The system daemon's
        // user_section is the admin baseline; the live user-config
        // catalog (and its per-profile signals) is owned by the
        // session daemon, which runs its own diff in
        // `Session1.ReloadConfig`. No signals fire from here.
        {
            let mut user = self.user_section.write().await;
            *user = new_config.user;
        }

        // Stage 3: re-resolve system profiles for all TVs.
        // The system section may have changed, so every TV's resolved profile
        // needs to be recomputed from the new system profiles.
        {
            let system = self.system_section.read().await;
            let tvs = self.tvs.read().await;
            for (_, tv_arc) in tvs.values() {
                let mut tv = tv_arc.lock().await;
                tv.system_profile = system.resolve_profile_by_uuid(tv.config.profile.system);
            }
        }

        // Notify signal subscribers that configuration changed
        // (profile lists, TV names, managed flags may all differ).
        self.emit_state_change(StateChangeEvent::ConfigReloaded);

        Ok(diff)
    }
}

/// Dispatch resolved [`SessionAction`]s via the backend, sharing the
/// action→primitive mapping with the session service (see
/// [`ActionExecutor`]).
#[async_trait::async_trait]
impl ActionExecutor for TvManager {
    type Error = TvManagerError;
    async fn screen_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.screen_on(tv_id).await
    }
    async fn screen_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.screen_off(tv_id).await
    }
    async fn power_on(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.power_on(tv_id).await
    }
    async fn power_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
        self.power_off(tv_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tisvlay_core::HardwareId;

    /// Minimal `TvConnection` fake for unit tests — only implements
    /// what `ManagedTv::is_connected()` needs and the trait's
    /// required methods. Everything else inherits the trait's
    /// `Err(not_supported)` defaults.
    struct FakeConnection {
        connected: bool,
    }

    #[async_trait]
    impl TvConnection for FakeConnection {
        fn vendor_id(&self) -> &'static str {
            "fake"
        }
        fn is_connected(&self) -> bool {
            self.connected
        }
        async fn get_state(&mut self) -> tisvlay_core::Result<TvState> {
            Ok(TvState::default())
        }
        async fn close(&mut self) -> tisvlay_core::Result<()> {
            self.connected = false;
            Ok(())
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn hardware_id(&self) -> Option<HardwareId> {
            None
        }
    }

    fn mk_tvs(
        tv_id: &str,
        connection: Option<Box<dyn TvConnection>>,
    ) -> Arc<RwLock<IndexMap<String, TvEntry>>> {
        // Derive a stable UUID from the human-readable test id so the
        // `TvConfig` field is well-typed, but key the in-memory map by
        // `tv_id` itself — tests assert on this string and don't care
        // about the config's internal UUID.
        let uuid = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, tv_id.as_bytes());
        let mut tv = ManagedTv::new(
            TvConfig::with_uuid(uuid, tv_id, "fake", true),
            ResolvedSystemProfile::default(),
        );
        tv.connection = connection;

        let mut map = IndexMap::new();
        map.insert(tv_id.to_string(), (true, Arc::new(Mutex::new(tv))));
        Arc::new(RwLock::new(map))
    }

    /// Regression: when the subscription's `state_rx` closes but the
    /// TV is still connected (a replacement connection's fresh
    /// subscription has taken over), the end-handler must NOT emit
    /// `ConnectionChanged(false)`. Emitting here would trigger
    /// downstream consumers to tear down per-TV resources with no
    /// subsequent event to re-create them.
    #[tokio::test]
    async fn subscription_end_suppresses_false_when_tv_still_connected() {
        let tvs = mk_tvs("office", Some(Box::new(FakeConnection { connected: true })));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = mpsc::unbounded_channel();
        drop(state_tx); // end of stream — subscription closed

        run_subscription_task("office".to_string(), tvs, tx, state_rx).await;

        assert!(
            rx.try_recv().is_err(),
            "subscription end with TV still connected must not emit any event"
        );
    }

    /// Complement to the regression above: when the TV is actually
    /// disconnected (connection gone), the subscription-end handler
    /// SHOULD emit `ConnectionChanged(false)` so state_forwarder can
    /// tear down the kernel device and downstream consumers know the
    /// TV is offline.
    #[tokio::test]
    async fn subscription_end_emits_false_when_tv_disconnected() {
        let tvs = mk_tvs(
            "office",
            Some(Box::new(FakeConnection { connected: false })),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = mpsc::unbounded_channel();
        drop(state_tx);

        run_subscription_task("office".to_string(), tvs, tx, state_rx).await;

        match rx.try_recv() {
            Ok(StateChangeEvent::ConnectionChanged { tv_id, connected }) => {
                assert_eq!(tv_id, "office");
                assert!(!connected);
            }
            other => panic!("expected ConnectionChanged(false), got {other:?}"),
        }
    }

    /// If the TV has been removed from the `tvs` map entirely (config
    /// reload, RemoveTv D-Bus call), the subscription-end handler
    /// returns early with just a debug log — emitting a ConnectionChanged
    /// for a TV whose D-Bus objects have been torn down is pointless.
    #[tokio::test]
    async fn subscription_end_on_removed_tv_emits_nothing() {
        let tvs: Arc<RwLock<IndexMap<String, TvEntry>>> = Arc::new(RwLock::new(IndexMap::new()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = mpsc::unbounded_channel();
        drop(state_tx);

        run_subscription_task("ghost_tv".to_string(), tvs, tx, state_rx).await;

        assert!(rx.try_recv().is_err());
    }

    /// `reload_config` snapshots the system-profile catalog before
    /// swapping in the new section and surfaces a three-way diff on
    /// the returned `ReloadDiff` so the dbus layer can fan out
    /// per-profile signals (Added / Changed / Removed) matching the
    /// equivalent external mutation calls.
    #[tokio::test]
    async fn reload_config_emits_system_profile_catalog_diff() {
        use crate::registry::BackendRegistry;
        use std::collections::BTreeMap;
        use tisvlay_core::AutomateWhen;
        use tisvlay_core::SystemSection;
        use uuid::Uuid;

        // Build an initial config with two profiles besides the
        // reserved default: one we'll keep unchanged, one we'll mutate.
        let unchanged = Uuid::new_v4();
        let mutated = Uuid::new_v4();
        let removed = Uuid::new_v4();

        let mut profiles = BTreeMap::new();
        profiles.insert(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileConfig::full_default(),
        );
        profiles.insert(
            unchanged,
            SystemProfileConfig {
                name: "unchanged".to_string(),
                automate_when: Some(AutomateWhen::Always),
                ..Default::default()
            },
        );
        profiles.insert(
            mutated,
            SystemProfileConfig {
                name: "mutated".to_string(),
                automate_when: Some(AutomateWhen::Always),
                ..Default::default()
            },
        );
        profiles.insert(
            removed,
            SystemProfileConfig {
                name: "to_remove".to_string(),
                ..Default::default()
            },
        );
        let mut config = SystemConfig {
            system: SystemSection { profiles },
            ..Default::default()
        };
        // `SystemConfig::load_from` normalizes the reserved-default
        // profile body (forces `name = "default"`). Apply the same
        // normalization to the seed config so the in-memory view
        // and the disk-load view start byte-identical — otherwise
        // the reserved default would spuriously surface as
        // `system_profiles_changed` on the first reload.
        config.system.ensure_default();
        config.user.ensure_default();

        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        config.save_to(&config_path).expect("save initial config");

        let registry = Arc::new(BackendRegistry::new());
        let (manager, _state_rx) = TvManager::new(config, config_path.clone(), registry);

        // Now mutate the config on disk: change `mutated`'s
        // `automate_when`, drop `removed`, insert a brand-new entry.
        let added_uuid = Uuid::new_v4();
        let mut new_profiles = BTreeMap::new();
        new_profiles.insert(
            DEFAULT_SYSTEM_PROFILE_UUID,
            SystemProfileConfig::full_default(),
        );
        new_profiles.insert(
            unchanged,
            SystemProfileConfig {
                name: "unchanged".to_string(),
                automate_when: Some(AutomateWhen::Always),
                ..Default::default()
            },
        );
        new_profiles.insert(
            mutated,
            SystemProfileConfig {
                name: "mutated".to_string(),
                // Field flip: signal callers should observe this UUID
                // in `system_profiles_changed`.
                automate_when: Some(AutomateWhen::OnPrimary),
                ..Default::default()
            },
        );
        new_profiles.insert(
            added_uuid,
            SystemProfileConfig {
                name: "fresh".to_string(),
                ..Default::default()
            },
        );
        let new_config = SystemConfig {
            system: SystemSection {
                profiles: new_profiles,
            },
            ..Default::default()
        };
        new_config
            .save_to(&config_path)
            .expect("save mutated config");

        let diff = manager.reload_config().await.expect("reload");

        assert_eq!(
            diff.system_profiles_added,
            vec![added_uuid.to_string()],
            "fresh profile must surface as Added",
        );
        assert_eq!(
            diff.system_profiles_changed,
            vec![mutated.to_string()],
            "mutated body must surface as Changed",
        );
        assert_eq!(
            diff.system_profiles_removed,
            vec![removed.to_string()],
            "removed profile must surface as Removed",
        );
        // `unchanged` and the reserved default both must be absent
        // from every diff list — equal bodies are no-ops.
        assert!(
            !diff
                .system_profiles_changed
                .contains(&unchanged.to_string()),
            "unchanged profile must not surface in Changed",
        );
        assert!(
            !diff
                .system_profiles_changed
                .contains(&DEFAULT_SYSTEM_PROFILE_UUID.to_string()),
            "reserved-default profile must not surface in Changed when body is byte-identical",
        );
    }
}
