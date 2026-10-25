//! Core traits for multi-vendor, transport-agnostic TV backend abstraction.
//!
//! This module defines the trait hierarchy that all TV vendor backends must implement.
//! The design follows a capability-based approach where backends advertise their
//! supported features via [`Capabilities`] bitflags.
//!
//! # Trait Hierarchy
//!
//! ```text
//! TvBackend (metadata: vendor_id, vendor_name, capabilities)
//!     │
//!     ├── TvConnector (factory: connect, wake, pair, display_address)
//!     │
//!     └── TvDiscovery (network discovery)
//!
//! TvConnection (base + feature methods with default not-supported impls)
//!     ├── power_off, get_power_state
//!     ├── screen_on, screen_off, is_screen_on
//!     ├── set_volume, get_volume, set_mute, is_muted
//!     ├── switch_input, get_current_input, list_inputs
//!     └── set_backlight, get_backlight
//! ```
//!
//! # Transport Agnosticism
//!
//! The connector interface is fully transport-agnostic. All transport details
//! (IP addresses, serial ports, CEC addresses, etc.) are encapsulated in the
//! [`VendorConfig`], which only the vendor crate knows how to interpret. The
//! daemon and CLI never see transport-specific types.
//!
//! # Async Style
//!
//! [`TvConnection`] and [`TvConnector`] use `#[async_trait]`, because the
//! daemon holds them as trait objects (`Box<dyn TvConnection>`,
//! `Arc<dyn TvConnector>`) and a native `async fn` in a trait isn't
//! dyn-compatible. [`TvDiscovery`] is consumed generically, so it uses a
//! native `impl Future` return; a caller that needs a trait object wraps it
//! in a boxing adapter of its own (the daemon's `DynDiscovery`). Implement
//! each trait the way it's declared: `#[async_trait]` on impls of the first
//! two, a plain `async fn` for `discover`.

use crate::{
    Capabilities, DiscoveredTv, Error, HardwareId, InputInfo, MacAddress, PairingSession,
    PowerState, Result, TvEvent, TvState, VendorConfig,
};
use async_trait::async_trait;
use std::net::IpAddr;

/// Communication transport used by a backend to connect to a device.
///
/// The daemon uses this to scope system events (e.g., NetworkManager
/// signals) to the appropriate devices — only network-dependent
/// connections are affected by network loss/restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// TCP/IP network (WebSocket, HTTP, raw TCP).
    Network,
    /// Serial port (RS-232, RS-485).
    Serial,
    /// USB device connection.
    Usb,
    /// HDMI-CEC (Consumer Electronics Control).
    Cec,
}

impl Transport {
    /// Whether this transport depends on network connectivity.
    ///
    /// Returns `true` for transports that go down when the network
    /// interface is lost (e.g., system sleep, NIC reset).
    pub fn is_network_dependent(self) -> bool {
        matches!(self, Self::Network)
    }
}

/// Core trait that all TV backends must implement.
///
/// This trait provides metadata about the backend (vendor identification and capabilities).
/// Backends should also implement [`TvConnector`] to enable creating connections to TVs.
///
/// # Example
///
/// ```ignore
/// pub struct LgBackend;
///
/// impl TvBackend for LgBackend {
///     fn vendor_id(&self) -> &'static str { "lg" }
///     fn vendor_name(&self) -> &'static str { "LG WebOS" }
///     fn capabilities(&self) -> Capabilities {
///         Capabilities::POWER | Capabilities::SCREEN | Capabilities::VOLUME
///             | Capabilities::MUTE | Capabilities::INPUT | Capabilities::BACKLIGHT
///             | Capabilities::WOL | Capabilities::DISCOVERY
///     }
/// }
/// ```
pub trait TvBackend: Send + Sync + 'static {
    /// Unique identifier for this backend (e.g., "lg", "samsung").
    ///
    /// This is used in configuration files and D-Bus interfaces to identify the vendor.
    fn vendor_id(&self) -> &'static str;

    /// Human-readable vendor name (e.g., "LG WebOS", "Samsung Tizen").
    ///
    /// This is displayed in user interfaces.
    fn vendor_name(&self) -> &'static str;

    /// Capabilities this backend supports.
    ///
    /// This determines which feature interfaces are available for TVs using this backend.
    fn capabilities(&self) -> Capabilities;

    /// Tuning for the daemon's verify-retry-escalate dispatch loop.
    ///
    /// Each backend reports the post-command verify delay and the
    /// max number of attempts that make sense for its transport.
    /// The daemon reads this once at dispatch time and applies the
    /// returned values to its `screen_on` / `screen_off` retry loop.
    ///
    /// Bundled into a single struct so adding a third tuning knob
    /// later (e.g., per-action overrides, escalation budget) doesn't
    /// churn the trait surface. Per-backend so future transports
    /// (USB-CEC, RS-232) with materially different retry semantics
    /// can override without touching the daemon.
    ///
    /// Default: LG-tuned (`300 ms` verify delay, `2` attempts). LG's
    /// `getPowerState` subscription is sub-100 ms; the 300 ms window
    /// gives the cache comfortable headroom to refresh after the
    /// state-change command lands. Two attempts cover the racy
    /// "TV processed the request but the PowerState push lagged"
    /// case; further retries are pointless once the TV has slipped
    /// past Screen Off — only WoL can recover.
    fn verify_retry_config(&self) -> RetryConfig {
        RetryConfig::default()
    }
}

/// Tuning for the daemon's verify-retry-escalate dispatch loop.
///
/// Per-backend via [`TvBackend::verify_retry_config`]. Default values
/// are LG-tuned; future backends override only if their state-push
/// semantics or wake budget diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryConfig {
    /// Wall-clock delay after a state-change command before the
    /// daemon reads cached PowerState back to verify the transition.
    pub verify_delay: std::time::Duration,
    /// Max attempts before the daemon escalates to `power_on`
    /// (for the wake-side `screen_on` path) or gives up (for the
    /// already-reachable `screen_off` retry path).
    pub max_attempts: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            verify_delay: std::time::Duration::from_millis(300),
            max_attempts: 2,
        }
    }
}

impl RetryConfig {
    /// `max_attempts` clamped to at least 1.
    ///
    /// Guards the dispatch hot path against a backend that declares
    /// `max_attempts = 0` either by accident or as a "best-effort
    /// only" stance: with zero attempts the verify-retry loop never
    /// runs and a literal-zero would let `screen_on` / `screen_off`
    /// return `Ok` without sending a single command. One attempt is
    /// the floor — backends that genuinely want fire-and-forget
    /// should still send the command once.
    pub fn effective_max_attempts(&self) -> usize {
        self.max_attempts.max(1)
    }
}

/// A local device node a connection depends on, such as an HDMI-CEC
/// adapter's `/dev/cecN`.
///
/// Returned by [`TvConnector::local_device`]. The daemon watches the
/// path, so a TV whose node is absent waits for it to appear instead of
/// dialing into it, and redials once it's back.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalDevice {
    /// The node's path as configured; a udev alias (a symlink) works.
    pub path: std::path::PathBuf,
}

impl LocalDevice {
    /// A local device at `path`.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// Connection to a specific TV instance.
///
/// This trait provides connection lifecycle management and all TV control methods.
/// Feature methods have default implementations that return [`Error::NotSupported`],
/// so backends only need to override the methods they support. Use
/// [`capabilities()`](TvConnection::capabilities) to check which features are
/// available before calling feature methods.
///
/// # Connection Resilience
///
/// Implementations should handle their own connection lifecycle internally.
/// For persistent-connection transports (e.g., WebSocket), the implementation
/// should transparently reconnect on stale or dropped connections when a
/// feature method is called. The daemon treats `TvConnection` as an opaque
/// handle — it does not manage reconnection or retry logic.
///
/// For `power_off()`, the implementation should close its own transport
/// connection after sending the command, since the device is shutting down.
///
/// `TvConnection` is the primary interface used by the daemon and CLI.
#[async_trait]
pub trait TvConnection: Send + Sync {
    /// Get the vendor ID for this connection's backend.
    fn vendor_id(&self) -> &'static str;

    /// Check if the connection is currently active.
    fn is_connected(&self) -> bool;

    /// Get the current TV state.
    ///
    /// This returns a snapshot of the TV's current state including power, volume,
    /// input, and other status information. The exact fields populated depend on
    /// the backend's capabilities.
    async fn get_state(&mut self) -> Result<TvState>;

    /// Close the connection gracefully.
    ///
    /// After calling this method, the connection should not be used further.
    async fn close(&mut self) -> Result<()>;

    /// Get capabilities supported by this connection.
    fn capabilities(&self) -> Capabilities;

    /// Vendor-reported hardware identity (manufacturer / model / serial).
    ///
    /// Returns `None` for backends that can't read identity from the
    /// device. Returning `Some` does not require every field to be
    /// populated — backends fill what they know, and matching falls
    /// back from `(manufacturer, model, serial)` → `(manufacturer,
    /// model)` when a serial isn't available.
    ///
    /// This is a read from cached state (populated at connect time),
    /// not an I/O call — synchronous by design so the daemon can read
    /// it while holding the connection lock without awaiting.
    fn hardware_id(&self) -> Option<HardwareId> {
        None
    }

    /// Canonical vendor id for branding the GUI vendor tile, when the
    /// backend can resolve the *device's own* vendor (CEC reads the TV's OUI
    /// → `lg`/`samsung`/…). `None` otherwise — the daemon then brands by the
    /// backend vendor id. A synchronous cached read (populated at connect
    /// time, like [`Self::hardware_id`]), so the daemon can read it under the
    /// connection lock without awaiting.
    fn brand(&self) -> Option<String> {
        None
    }

    /// The MAC of the interface this connection reached the TV through, as
    /// the device itself reports it. Read once when a pairing completes, to
    /// fill a missing Wake-on-LAN MAC: it works for a TV off the daemon's
    /// link (a routed or VLAN-isolated segment), where the kernel neighbor
    /// table only ever sees the gateway.
    ///
    /// Best-effort: `None` when the device can't say, the query fails, or
    /// the reported interfaces don't match the dialed address. Implement
    /// it only if the backend's [`MAC_CONFIG_KEY`](crate::MAC_CONFIG_KEY)
    /// config field drives wake.
    async fn get_link_mac(&mut self) -> Option<MacAddress> {
        None
    }

    // =========================================================================
    // Power control (default: not supported)
    // =========================================================================

    /// Power off the TV (standby).
    async fn power_off(&mut self) -> Result<()> {
        Err(Error::not_supported("power_off", Capabilities::POWER))
    }

    /// Wake the TV on this *existing* connection (e.g. CEC Image View On).
    ///
    /// Only meaningful for backends that stay reachable in standby (see
    /// [`TvConnector::reachable_in_standby`]): the connection is still live
    /// while the TV sleeps, so the daemon can wake it in place instead of
    /// tearing the connection down and opening a fresh link via
    /// [`TvConnector::wake_and_connect`] — the teardown would flap
    /// `Connected` (a spurious disconnect on a power-*on*). Default: not
    /// supported, so those callers fall back to `wake_and_connect`.
    async fn power_on(&mut self) -> Result<()> {
        Err(Error::not_supported("power_on", Capabilities::POWER))
    }

    /// Get the current power state.
    async fn get_power_state(&mut self) -> Result<PowerState> {
        Err(Error::not_supported("get_power_state", Capabilities::POWER))
    }

    // =========================================================================
    // Screen control (default: not supported)
    // =========================================================================

    /// Turn the screen on (unblank).
    async fn screen_on(&mut self) -> Result<()> {
        Err(Error::not_supported("screen_on", Capabilities::SCREEN))
    }

    /// Turn the screen off (blank).
    ///
    /// The TV remains powered on but the display is blanked.
    /// This is OLED-safe and doesn't require re-pairing like power off might.
    async fn screen_off(&mut self) -> Result<()> {
        Err(Error::not_supported("screen_off", Capabilities::SCREEN))
    }

    /// Check if the screen is currently on.
    async fn is_screen_on(&mut self) -> Result<bool> {
        Err(Error::not_supported("is_screen_on", Capabilities::SCREEN))
    }

    // =========================================================================
    // Volume control (default: not supported)
    // =========================================================================

    /// Set the volume level.
    ///
    /// `level` must be in the range 0-100. Values above 100 will be clamped
    /// by backend implementations.
    async fn set_volume(&mut self, _level: u8) -> Result<()> {
        Err(Error::not_supported("set_volume", Capabilities::VOLUME))
    }

    /// Get the current volume level (0-100).
    async fn get_volume(&mut self) -> Result<u8> {
        Err(Error::not_supported("get_volume", Capabilities::VOLUME))
    }

    /// Set the mute state.
    async fn set_mute(&mut self, _mute: bool) -> Result<()> {
        Err(Error::not_supported("set_mute", Capabilities::MUTE))
    }

    /// Check if audio is currently muted.
    async fn is_muted(&mut self) -> Result<bool> {
        Err(Error::not_supported("is_muted", Capabilities::MUTE))
    }

    // =========================================================================
    // Input control (default: not supported)
    // =========================================================================

    /// Switch to the specified input.
    ///
    /// The `input_id` format is backend-specific (e.g., "HDMI_1" for LG WebOS).
    async fn switch_input(&mut self, _input_id: &str) -> Result<()> {
        Err(Error::not_supported("switch_input", Capabilities::INPUT))
    }

    /// Get the current input ID, if known.
    async fn get_current_input(&mut self) -> Result<Option<String>> {
        Err(Error::not_supported(
            "get_current_input",
            Capabilities::INPUT,
        ))
    }

    /// List available inputs.
    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>> {
        Err(Error::not_supported("list_inputs", Capabilities::INPUT))
    }

    // =========================================================================
    // Backlight control (default: not supported)
    // =========================================================================

    /// Set the backlight level (OLED pixel brightness).
    ///
    /// `level` must be in the range 0-100. Values above 100 will be clamped
    /// by backend implementations.
    async fn set_backlight(&mut self, _level: u8) -> Result<()> {
        Err(Error::not_supported(
            "set_backlight",
            Capabilities::BACKLIGHT,
        ))
    }

    /// Get the current backlight level (0-100).
    async fn get_backlight(&mut self) -> Result<u8> {
        Err(Error::not_supported(
            "get_backlight",
            Capabilities::BACKLIGHT,
        ))
    }

    // =========================================================================
    // State subscriptions (default: not supported)
    // =========================================================================

    /// Subscribe to state change notifications from the device.
    ///
    /// Returns a channel that receives [`TvState`] snapshots whenever the
    /// device reports a change (e.g., volume adjusted via TV remote).
    /// Each snapshot reflects the cumulative state at the time of the
    /// update — not just the changed field.
    ///
    /// Backends that don't support push-based state return `NotSupported`.
    async fn subscribe_state(&mut self) -> Result<tokio::sync::mpsc::UnboundedReceiver<TvState>> {
        Err(Error::not_supported(
            "subscribe_state",
            Capabilities::empty(),
        ))
    }

    /// Subscribe to the TV's [`TvEvent`]s: what it did, rather than what
    /// state it's in (that's [`subscribe_state`](Self::subscribe_state)).
    ///
    /// The channel ending means only that no more events come; the
    /// connection's liveness is the state subscription's. A backend
    /// reporting an event declares its capability (for
    /// [`TvEvent::RemotePower`], [`Capabilities::REMOTE_POWER`]).
    ///
    /// Backends with no events return `NotSupported`.
    async fn subscribe_events(&mut self) -> Result<tokio::sync::mpsc::UnboundedReceiver<TvEvent>> {
        Err(Error::not_supported(
            "subscribe_events",
            Capabilities::REMOTE_POWER,
        ))
    }

    /// Let the TV remote's keys reach the desktop, or stop them: the system
    /// profile's `remote_keys`, applied to a live connection whenever it
    /// changes. The keys go through the host's input stack, which decides
    /// what each one does.
    ///
    /// Backends without [`Capabilities::REMOTE_KEYS`] return `NotSupported`.
    async fn set_remote_keys(&mut self, _on: bool) -> Result<()> {
        Err(Error::not_supported(
            "set_remote_keys",
            Capabilities::REMOTE_KEYS,
        ))
    }
}

/// Factory for creating connections to TVs.
///
/// This trait is implemented by backends to provide connection creation, wake-up,
/// pairing, and transport metadata functionality. All transport details (IP, MAC,
/// serial port, CEC address, etc.) are encapsulated in the
/// [`VendorConfig`], which only the vendor crate knows how to interpret.
#[async_trait]
pub trait TvConnector: Send + Sync {
    /// The communication transport used by this connector.
    fn transport(&self) -> Transport;

    /// Whether connections from this backend survive client source IP changes.
    ///
    /// TCP/TLS-based protocols (WebSocket, raw TCP) return `false` — the
    /// connection is bound to a 4-tuple and dies when the source IP changes.
    /// QUIC-based protocols may return `true` if connection migration is
    /// supported. Stateless protocols (HTTP request-response) may also
    /// return `true`.
    ///
    /// Only called for network-dependent transports (gated by
    /// `transport().is_network_dependent()`). The daemon uses this when
    /// the route to a TV shifts to a different NIC: if `false`, the
    /// existing connection is closed and a new one established via the
    /// new interface.
    fn survives_source_ip_change(&self) -> bool {
        false
    }

    /// Whether the transport keeps a live connection while the TV is in
    /// standby — i.e. whether `Connected` and the power state are *separate*
    /// axes for this backend.
    ///
    /// CEC returns `true`: the HDMI-CEC bus stays reachable in standby (we
    /// can still query power and receive the TV's broadcasts), so a
    /// standby TV is legitimately `Connected` with `Power = standby`, and
    /// `power_on` wakes it based on the reported power state rather than on
    /// whether a fresh connect succeeds. Network backends (LG) return the
    /// default `false`: the socket dies on power-off, so the daemon drops
    /// the connection and `Connected` tracks power.
    fn reachable_in_standby(&self, _vendor_config: &VendorConfig) -> bool {
        false
    }

    /// Whether this backend fulfills screen-blank actions by powering the
    /// device — i.e. emulates the `SCREEN` capability via `POWER`.
    ///
    /// For a device with `POWER` but no discrete screen-blank (HDMI-CEC: the
    /// panel can't go dark while the TV stays on), returning `true` lets the
    /// daemon advertise `SCREEN` and dispatch `screen_off`/`screen_on` as
    /// `power_off`/`power_on` — so the stock screen-action profiles
    /// (idle/lock/DPMS) behave consistently across a mixed fleet without the
    /// user rewriting them to power actions. The mapping lives at the daemon
    /// dispatch (not the connection) so it reuses the correct power path
    /// (standby-cache synthesis, wake-in-place). Default `false`: a backend
    /// with a real screen-blank, or one that should simply ignore screen
    /// actions, leaves it off. Config-driven so it's opt-out per device.
    fn emulate_screen_via_power(&self, _vendor_config: &VendorConfig) -> bool {
        false
    }

    /// The local device node this TV's connection needs, if any.
    ///
    /// A backend that talks through a node on this host (HDMI-CEC's
    /// `/dev/cecN`) returns it, and the daemon then treats the node like
    /// a network TV's route: while it's absent the dial waits instead of
    /// failing, and its return redials. Default `None`: network backends
    /// depend on a route, which the daemon resolves from
    /// [`primary_descriptor`](Self::primary_descriptor).
    fn local_device(&self, _vendor_config: &VendorConfig) -> Option<LocalDevice> {
        None
    }

    /// Connect to a TV using vendor-specific config.
    ///
    /// The vendor config contains all transport and credential parameters.
    /// The connector deserializes it into its own config type internally.
    async fn connect(&self, vendor_config: &VendorConfig) -> Result<Box<dyn TvConnection>>;

    /// Vendor-specific connection info for the GUI's INFO pane.
    ///
    /// User-editable vendor-config fields, ordered `(label, value)`
    /// pairs. The GUI's TVs-pane CONFIG group uses this for its
    /// view-mode read-only display, and the edit pane uses the
    /// same field set as its editable surface (driven by the
    /// `UserInput` slice of [`config_schema`](Self::config_schema)).
    ///
    /// **Default impl** walks `config_schema` and emits a row per
    /// `UserInput`-visibility field. Empty values are elided.
    ///
    /// Backends only need to override this when the schema-driven
    /// projection isn't enough (e.g. the field's wire form needs
    /// reformatting before display — `0xFFEE...` → `FF:EE:...`).
    fn display_config(&self, vendor_config: &VendorConfig) -> Vec<(String, String)> {
        display_rows_for_visibility(
            &self.config_schema(),
            vendor_config,
            crate::FieldVisibility::UserInput,
        )
    }

    /// Backend's primary descriptor — the value of the first
    /// `UserInput`-visibility entry from the vendor config.
    /// Convention: backends order their `config_schema` so the most
    /// informative editable field (LG: ip; future Bluetooth: MAC;
    /// USB-CEC: serial port) comes first. Used by daemon-internal log
    /// formatters and route resolution where a single
    /// transport-appropriate string is needed.
    ///
    /// Returns `None` when the backend declines to surface any
    /// editable info.
    fn primary_descriptor(&self, vendor_config: &VendorConfig) -> Option<String> {
        self.display_config(vendor_config)
            .into_iter()
            .next()
            .map(|(_, value)| value)
    }

    /// Backend-discovered diagnostic fields, ordered
    /// `(label, value)` pairs. The GUI's TVs-pane INFO group uses
    /// this for its read-only diagnostic rows (LG: WebSocket
    /// scheme captured at pair time). These fields can only be
    /// refreshed via re-pair, so they're never editable.
    ///
    /// **Default impl** walks `config_schema` and emits a row per
    /// `Diagnostic`-visibility field. Empty values are elided.
    fn display_diagnostics(&self, vendor_config: &VendorConfig) -> Vec<(String, String)> {
        display_rows_for_visibility(
            &self.config_schema(),
            vendor_config,
            crate::FieldVisibility::Diagnostic,
        )
    }

    /// Whether this transport supports waking the device from standby.
    ///
    /// For example, IP-based backends may support Wake-on-LAN if a MAC
    /// address is configured, while CEC backends might support power-on.
    fn supports_wake(&self, vendor_config: &VendorConfig) -> bool;

    /// Wake the device from standby.
    ///
    /// For IP-based backends, this sends Wake-on-LAN. For CEC, this might
    /// send a power-on command. Returns `Ok(())` as a no-op if wake is
    /// not supported.
    ///
    /// Implementations should handle transport-specific retry logic internally.
    /// For example, after system resume the desktop's network interface may not
    /// be up yet, so WoL packets should be retried until the network is available.
    async fn wake(&self, vendor_config: &VendorConfig) -> Result<()>;

    /// Wake the device and establish a connection.
    ///
    /// The backend owns all transport-specific retry logic: timing, retry
    /// count, wake packet resends, readiness probes, etc. The daemon does
    /// not provide any timing parameters — each backend knows its own
    /// transport characteristics.
    async fn wake_and_connect(&self, vendor_config: &VendorConfig)
    -> Result<Box<dyn TvConnection>>;

    /// Start pairing with a new device.
    ///
    /// The vendor config provides transport address and any initial connection
    /// parameters needed to reach the device.
    ///
    /// Returns a [`PairingSession`] that can be used to track pairing progress
    /// and submit user input if required by the backend's pairing flow.
    fn start_pairing(&self, vendor_config: &VendorConfig) -> Result<PairingSession>;

    /// Build vendor config from pairing results for persistence.
    ///
    /// After pairing completes with [`Credentials`](crate::Credentials), this
    /// method constructs the full vendor config the daemon persists in its
    /// store. `pairing_params` contains the initial
    /// parameters used to reach the device.
    fn build_config_from_pairing(
        &self,
        pairing_params: &VendorConfig,
        credentials: &crate::Credentials,
    ) -> Result<VendorConfig>;

    /// Full persisted-config schema — every field that lives in
    /// the vendor's TOML config, with a
    /// [`visibility`](crate::FieldVisibility) tag per field.
    /// Canonical source of truth for the schema views below.
    ///
    /// The framework projects this into:
    /// - **Pairing wizard** ([`pairing_schema`](Self::pairing_schema)):
    ///   `UserInput` fields only.
    /// - **INFO pane** ([`display_config`](Self::display_config) +
    ///   [`display_diagnostics`](Self::display_diagnostics)):
    ///   `UserInput + Diagnostic`, skips `Secret`.
    /// - **Edit pane** (future): `UserInput` only.
    ///
    /// Default impl returns an empty list — backends that haven't
    /// migrated to schema-driven config keep their previous
    /// behavior (no INFO-pane rows, no edit support). Override to
    /// describe the full vendor-config shape and tag each field's
    /// visibility; the framework handles projection from there.
    fn config_schema(&self) -> Vec<crate::PairingField> {
        Vec::new()
    }

    /// Pairing form schema — declares the input fields the user
    /// must provide before pairing can start.
    ///
    /// The daemon serves this as JSON from its
    /// `manager/vendors/<id>/schema` queryable; the GUI's
    /// pairing wizard renders the form dynamically from the
    /// returned [`PairingField`](crate::PairingField) list. See
    /// [`PairingFieldType`](crate::PairingFieldType) and [`FieldVisibility`](crate::FieldVisibility) for the field
    /// vocabulary.
    ///
    /// Default impl filters [`config_schema`](Self::config_schema)
    /// to `UserInput`-visibility fields. Backends that override
    /// `config_schema` get this projection for free; backends that
    /// haven't migrated can still override this directly.
    fn pairing_schema(&self) -> Vec<crate::PairingField> {
        self.config_schema()
            .into_iter()
            .filter(|f| f.visibility == crate::FieldVisibility::UserInput)
            .collect()
    }

    /// Send a probe to `ip` that primes the kernel ARP/NDP cache as
    /// a side effect. Best-effort: the daemon uses this only as a
    /// fallback when passive netlink lookup misses, and treats
    /// `Err(_)` as non-fatal — it still re-reads the cache afterward
    /// (another process may have primed the entry concurrently).
    ///
    /// Default no-op. Vendors that talk to an IP-addressed device
    /// (LG, future Sony/Samsung) should override to send a single
    /// transport-layer datagram or open a TCP connection that
    /// triggers the kernel's neighbor resolution on the way out.
    ///
    /// IP family agnostic by signature — the daemon's family gate
    /// uses the vendor's `Capabilities::IPV4` / `IPV6` bits to decide
    /// which family inputs reach the probe, so vendors can declare
    /// their transport's coverage without reshaping the trait.
    async fn probe_arp_cache(&self, _ip: IpAddr) -> Result<()> {
        Ok(())
    }
}

/// Project a `(label, value)` row list out of a vendor schema,
/// keeping only fields whose [`visibility`](crate::FieldVisibility)
/// matches the requested filter and whose value is non-empty.
/// Shared between [`TvConnector::display_config`] and
/// [`TvConnector::display_diagnostics`].
fn display_rows_for_visibility(
    schema: &[crate::PairingField],
    vendor_config: &VendorConfig,
    visibility: crate::FieldVisibility,
) -> Vec<(String, String)> {
    schema
        .iter()
        .filter(|f| f.visibility == visibility)
        .filter_map(|f| {
            // Fall back to the schema `default` when the field is absent from
            // the stored config: a field left at its serde default isn't
            // written to `[tv.<vendor>]`, so reading the stored value alone
            // would drop it from the read-only view — even though it has an
            // effective value the edit form (which prefills the default) does
            // show. Showing the default keeps view mode and edit mode
            // consistent. (Schema defaults are authored to match the serde
            // defaults, so this is the value actually in force.)
            let value =
                field_as_string(vendor_config, &f.name).unwrap_or_else(|| f.default.clone());
            if value.is_empty() {
                return None;
            }
            Some((f.label.clone(), value))
        })
        .collect()
}

/// Look up a vendor-config field by name and stringify its scalar value.
/// Returns `None` for missing keys or container types (arrays / tables).
fn field_as_string(config: &VendorConfig, name: &str) -> Option<String> {
    match config.get(name)? {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Datetime(d) => Some(d.to_string()),
        // Arrays / tables aren't meaningful as a single display
        // row — skip silently. Backends needing structured
        // display can override `display_config` directly.
        toml::Value::Array(_) | toml::Value::Table(_) => None,
    }
}

/// Discovery of TVs on the network.
///
/// Backends that support the [`Capabilities::DISCOVERY`] flag should implement this trait.
pub trait TvDiscovery: Send + Sync {
    /// Discover TVs on the local network.
    ///
    /// This scans the network for TVs supported by this backend and returns
    /// information about each discovered TV.
    fn discover(
        &self,
        timeout: std::time::Duration,
    ) -> impl Future<Output = Result<Vec<DiscoveredTv>>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Compile-time test that traits are object-safe
    // (TvDiscovery uses native async fn with impl Future and is NOT object-safe)
    fn _assert_object_safe() {
        fn _backend(_: &dyn TvBackend) {}
        fn _connection(_: &dyn TvConnection) {}
        fn _connector(_: &dyn TvConnector) {}
    }

    /// Stub `TvBackend` that takes the trait defaults for every method
    /// it can. Used to verify [`RetryConfig::default`] resolves to the
    /// documented LG-tuned values when a backend doesn't override.
    struct DefaultBackend;

    impl TvBackend for DefaultBackend {
        fn vendor_id(&self) -> &'static str {
            "default-stub"
        }
        fn vendor_name(&self) -> &'static str {
            "Default Stub"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
    }

    /// Stub backend that overrides `verify_retry_config`, to confirm
    /// the override path compiles and dispatches correctly.
    struct PatientBackend;

    impl TvBackend for PatientBackend {
        fn vendor_id(&self) -> &'static str {
            "patient-stub"
        }
        fn vendor_name(&self) -> &'static str {
            "Patient Stub"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn verify_retry_config(&self) -> RetryConfig {
            RetryConfig {
                verify_delay: Duration::from_millis(750),
                max_attempts: 5,
            }
        }
    }

    #[test]
    fn retry_config_default_matches_documented_lg_tuning() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.verify_delay, Duration::from_millis(300));
        assert_eq!(cfg.max_attempts, 2);
    }

    /// `effective_max_attempts()` clamps `max_attempts == 0` up to
    /// `1`. Guards the dispatch hot path against a backend that
    /// declares zero attempts either by accident or as a
    /// "best-effort only" stance — the verify-retry loop must run
    /// at least once or `screen_on` / `screen_off` would return
    /// `Ok` without sending a single command.
    #[test]
    fn effective_max_attempts_clamps_zero_up_to_one() {
        assert_eq!(
            RetryConfig {
                verify_delay: Duration::from_millis(300),
                max_attempts: 0,
            }
            .effective_max_attempts(),
            1,
        );
        assert_eq!(
            RetryConfig {
                verify_delay: Duration::from_millis(300),
                max_attempts: 5,
            }
            .effective_max_attempts(),
            5,
        );
    }

    #[test]
    fn verify_retry_config_default_resolves_to_lg_tuning() {
        let backend = DefaultBackend;
        let cfg = backend.verify_retry_config();
        assert_eq!(cfg.verify_delay, Duration::from_millis(300));
        assert_eq!(cfg.max_attempts, 2);
    }

    #[test]
    fn verify_retry_config_backend_can_override() {
        let backend = PatientBackend;
        let cfg = backend.verify_retry_config();
        assert_eq!(cfg.verify_delay, Duration::from_millis(750));
        assert_eq!(cfg.max_attempts, 5);
    }

    /// Dispatching via `&dyn TvBackend` resolves to the override, not
    /// the default — pins that `verify_retry_config` is virtual, so
    /// the daemon's `registry.get(vendor).backend.verify_retry_config()`
    /// call path sees per-backend values.
    #[test]
    fn verify_retry_config_dispatches_virtually() {
        let patient: Box<dyn TvBackend> = Box::new(PatientBackend);
        assert_eq!(
            patient.verify_retry_config().verify_delay,
            Duration::from_millis(750)
        );
        let default: Box<dyn TvBackend> = Box::new(DefaultBackend);
        assert_eq!(
            default.verify_retry_config().verify_delay,
            Duration::from_millis(300)
        );
    }
}
