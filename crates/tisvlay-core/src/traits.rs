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
//! vendor config (`toml::Value`), which only the vendor crate knows how to
//! interpret. The daemon and CLI never see transport-specific types.

use crate::{
    Capabilities, DiscoveredTv, Error, HardwareId, InputInfo, PairingSession, PowerState, Result,
    TvState,
};
use async_trait::async_trait;

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

    // =========================================================================
    // Power control (default: not supported)
    // =========================================================================

    /// Power off the TV (standby).
    async fn power_off(&mut self) -> Result<()> {
        Err(Error::not_supported("power_off", Capabilities::POWER))
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
}

/// Factory for creating connections to TVs.
///
/// This trait is implemented by backends to provide connection creation, wake-up,
/// pairing, and transport metadata functionality. All transport details (IP, MAC,
/// serial port, CEC address, etc.) are encapsulated in the vendor config
/// (`toml::Value`), which only the vendor crate knows how to interpret.
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

    /// Connect to a TV using vendor-specific config.
    ///
    /// The vendor config contains all transport and credential parameters.
    /// The connector deserializes it into its own config type internally.
    async fn connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>>;

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
    fn display_config(&self, vendor_config: &toml::Value) -> Vec<(String, String)> {
        display_rows_for_visibility(
            &self.config_schema(),
            vendor_config,
            crate::FieldVisibility::UserInput,
        )
    }

    /// Backend-discovered diagnostic fields, ordered
    /// `(label, value)` pairs. The GUI's TVs-pane INFO group uses
    /// this for its read-only diagnostic rows (LG: WebSocket
    /// scheme captured at pair time). These fields can only be
    /// refreshed via re-pair, so they're never editable.
    ///
    /// **Default impl** walks `config_schema` and emits a row per
    /// `Diagnostic`-visibility field. Empty values are elided.
    fn display_diagnostics(&self, vendor_config: &toml::Value) -> Vec<(String, String)> {
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
    fn supports_wake(&self, vendor_config: &toml::Value) -> bool;

    /// Wake the device from standby.
    ///
    /// For IP-based backends, this sends Wake-on-LAN. For CEC, this might
    /// send a power-on command. Returns `Ok(())` as a no-op if wake is
    /// not supported.
    ///
    /// Implementations should handle transport-specific retry logic internally.
    /// For example, after system resume the desktop's network interface may not
    /// be up yet, so WoL packets should be retried until the network is available.
    async fn wake(&self, vendor_config: &toml::Value) -> Result<()>;

    /// Wake the device and establish a connection.
    ///
    /// The backend owns all transport-specific retry logic: timing, retry
    /// count, wake packet resends, readiness probes, etc. The daemon does
    /// not provide any timing parameters — each backend knows its own
    /// transport characteristics.
    async fn wake_and_connect(&self, vendor_config: &toml::Value) -> Result<Box<dyn TvConnection>>;

    /// Start pairing with a new device.
    ///
    /// The vendor config provides transport address and any initial connection
    /// parameters needed to reach the device.
    ///
    /// Returns a [`PairingSession`] that can be used to track pairing progress
    /// and submit user input if required by the backend's pairing flow.
    fn start_pairing(&self, vendor_config: &toml::Value) -> Result<PairingSession>;

    /// Build vendor config from pairing results for persistence.
    ///
    /// After pairing completes with [`Credentials`](crate::Credentials), this
    /// method constructs the full vendor config (as a TOML value) that can be
    /// saved to the config file. `pairing_params` contains the initial
    /// parameters used to reach the device.
    fn build_config_from_pairing(
        &self,
        pairing_params: &toml::Value,
        credentials: &crate::Credentials,
    ) -> Result<toml::Value>;

    /// Full persisted-config schema — every field that lives in
    /// the vendor's TOML config, with a
    /// [`visibility`](crate::FieldVisibility) tag per field.
    /// Canonical source of truth for the schema views below.
    ///
    /// The framework projects this into:
    /// - **Pairing wizard** ([`pairing_schema`](Self::pairing_schema)):
    ///   `UserInput` fields only.
    /// - **INFO pane** ([`display_info`](Self::display_info)):
    ///   `UserInput + Diagnostic`, skips `Secret`.
    /// - **Edit pane** (future): `UserInput` only.
    ///
    /// Default impl returns an empty list — backends that haven't
    /// migrated to schema-driven config keep their previous
    /// behavior (no display_info, no edit support). Override to
    /// describe the full vendor-config shape and tag each field's
    /// visibility; the framework handles projection from there.
    fn config_schema(&self) -> Vec<crate::PairingField> {
        Vec::new()
    }

    /// Pairing form schema — declares the input fields the user
    /// must provide before pairing can start.
    ///
    /// The daemon exposes this over D-Bus
    /// (`Manager1.GetPairingSchema(vendor)`) as JSON; the GUI's
    /// pairing wizard renders the form dynamically from the
    /// returned [`PairingField`] list. See
    /// [`pairing_schema`](crate::pairing_schema) for the field
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
}

/// Look up a field on a TOML table by name and stringify its
/// scalar value. Returns `None` for missing keys, `null`, or
/// container types (arrays / tables). Used by the default
/// [`TvConnector::display_info`] impl to project values out of a
/// `vendor_config` for display.
/// Project a `(label, value)` row list out of a vendor schema,
/// keeping only fields whose [`visibility`](crate::FieldVisibility)
/// matches the requested filter and whose value is non-empty.
/// Shared between [`TvConnector::display_config`] and
/// [`TvConnector::display_diagnostics`].
fn display_rows_for_visibility(
    schema: &[crate::PairingField],
    vendor_config: &toml::Value,
    visibility: crate::FieldVisibility,
) -> Vec<(String, String)> {
    schema
        .iter()
        .filter(|f| f.visibility == visibility)
        .filter_map(|f| {
            let value = toml_field_as_string(vendor_config, &f.name)?;
            if value.is_empty() {
                return None;
            }
            Some((f.label.clone(), value))
        })
        .collect()
}

fn toml_field_as_string(value: &toml::Value, name: &str) -> Option<String> {
    let table = value.as_table()?;
    match table.get(name)? {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Datetime(d) => Some(d.to_string()),
        // Arrays / tables aren't meaningful as a single display
        // row — skip silently. Backends needing structured
        // display can override `display_info` directly.
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
    ) -> impl std::future::Future<Output = Result<Vec<DiscoveredTv>>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time test that traits are object-safe
    // (TvDiscovery uses native async fn with impl Future and is NOT object-safe)
    fn _assert_object_safe() {
        fn _backend(_: &dyn TvBackend) {}
        fn _connection(_: &dyn TvConnection) {}
        fn _connector(_: &dyn TvConnector) {}
    }
}
