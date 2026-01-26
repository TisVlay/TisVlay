//! Core traits for multi-vendor TV backend abstraction.
//!
//! This module defines the trait hierarchy that all TV vendor backends must implement.
//! The design follows a capability-based approach where backends advertise their
//! supported features via [`Capabilities`] bitflags, and feature-specific traits
//! provide the actual functionality.
//!
//! # Trait Hierarchy
//!
//! ```text
//! TvBackend (metadata: vendor_id, vendor_name, capabilities)
//!     │
//!     ├── TvConnector (factory: connect, start_pairing)
//!     │
//!     └── TvDiscovery (network discovery)
//!
//! TvConnection (base: is_connected, get_state, close)
//!     │
//!     ├── PowerControl (power_on, power_off)
//!     ├── ScreenControl (screen_on, screen_off) - OLED-safe blanking
//!     ├── VolumeControl (volume, mute)
//!     ├── InputControl (switch_input, list_inputs)
//!     └── BacklightControl (OLED pixel brightness)
//! ```

use crate::{Capabilities, DiscoveredTv, InputInfo, PairingSession, PowerState, Result, TvState};
use async_trait::async_trait;
use std::any::Any;

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
/// This is the base trait for all TV connections. It provides connection status,
/// state retrieval, and connection lifecycle management.
///
/// Feature-specific traits ([`PowerControl`], [`ScreenControl`], etc.) extend this
/// trait to provide additional functionality based on backend capabilities.
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

    /// Downcast to a concrete type for vendor-specific operations.
    ///
    /// This is used internally by the daemon to access vendor-specific
    /// functionality that doesn't fit the common trait interface.
    fn as_any(&self) -> &dyn Any;

    /// Downcast to a mutable concrete type for vendor-specific operations.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Power control capability.
///
/// Backends that support the [`Capabilities::POWER`] flag should implement this trait.
#[async_trait]
pub trait PowerControl: TvConnection {
    /// Power on the TV.
    ///
    /// For TVs that support Wake-on-LAN, this typically sends a WoL magic packet.
    /// The actual implementation depends on the backend.
    async fn power_on(&mut self) -> Result<()>;

    /// Power off the TV.
    ///
    /// This puts the TV into standby mode.
    async fn power_off(&mut self) -> Result<()>;

    /// Get the current power state.
    async fn get_power_state(&mut self) -> Result<PowerState>;
}

/// Screen blanking capability (OLED-safe).
///
/// Backends that support the [`Capabilities::SCREEN`] flag should implement this trait.
/// This is distinct from power control - screen off keeps the TV "on" but with the
/// display blanked, which is safer for OLED panels than full power off/on cycles.
#[async_trait]
pub trait ScreenControl: TvConnection {
    /// Turn the screen on (unblank).
    async fn screen_on(&mut self) -> Result<()>;

    /// Turn the screen off (blank).
    ///
    /// The TV remains powered on but the display is blanked.
    /// This is OLED-safe and doesn't require re-pairing like power off might.
    async fn screen_off(&mut self) -> Result<()>;

    /// Check if the screen is currently on.
    async fn is_screen_on(&mut self) -> Result<bool>;
}

/// Volume control capability.
///
/// Backends that support the [`Capabilities::VOLUME`] flag should implement this trait.
#[async_trait]
pub trait VolumeControl: TvConnection {
    /// Set the volume level (0-100).
    async fn set_volume(&mut self, level: u8) -> Result<()>;

    /// Get the current volume level (0-100).
    async fn get_volume(&mut self) -> Result<u8>;

    /// Set the mute state.
    async fn set_mute(&mut self, mute: bool) -> Result<()>;

    /// Check if audio is currently muted.
    async fn is_muted(&mut self) -> Result<bool>;
}

/// Input switching capability.
///
/// Backends that support the [`Capabilities::INPUT`] flag should implement this trait.
#[async_trait]
pub trait InputControl: TvConnection {
    /// Switch to the specified input.
    ///
    /// The `input_id` format is backend-specific (e.g., "HDMI_1" for LG WebOS).
    async fn switch_input(&mut self, input_id: &str) -> Result<()>;

    /// Get the current input ID, if known.
    async fn get_current_input(&mut self) -> Result<Option<String>>;

    /// List available inputs.
    async fn list_inputs(&mut self) -> Result<Vec<InputInfo>>;
}

/// Backlight/brightness control (OLED pixel brightness).
///
/// Backends that support the [`Capabilities::BACKLIGHT`] flag should implement this trait.
/// For OLED TVs, this controls the pixel brightness level.
#[async_trait]
pub trait BacklightControl: TvConnection {
    /// Set the backlight level (0-100).
    async fn set_backlight(&mut self, level: u8) -> Result<()>;

    /// Get the current backlight level (0-100).
    async fn get_backlight(&mut self) -> Result<u8>;
}

/// Factory for creating connections to TVs.
///
/// This trait is implemented by backends to provide connection creation and
/// pairing initiation functionality.
#[async_trait]
pub trait TvConnector: Send + Sync {
    /// Connect to a TV with existing credentials.
    ///
    /// The `vendor_config` parameter provides backend-specific configuration
    /// (e.g., `LgConfig` for LG WebOS with client_key).
    async fn connect(
        &self,
        ip: &str,
        mac: Option<&str>,
        vendor_config: &(dyn Any + Send + Sync),
    ) -> Result<Box<dyn TvConnection>>;

    /// Start pairing with a new TV.
    ///
    /// This returns a [`PairingSession`] that can be used to track the pairing
    /// progress and submit user input if required by the backend's pairing flow.
    fn start_pairing(&self, ip: &str) -> PairingSession;
}

/// Discovery of TVs on the network.
///
/// Backends that support the [`Capabilities::DISCOVERY`] flag should implement this trait.
#[async_trait]
pub trait TvDiscovery: Send + Sync {
    /// Discover TVs on the local network.
    ///
    /// This scans the network for TVs supported by this backend and returns
    /// information about each discovered TV.
    async fn discover(&self, timeout: std::time::Duration) -> Result<Vec<DiscoveredTv>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time test that traits are object-safe
    fn _assert_object_safe() {
        fn _backend(_: &dyn TvBackend) {}
        fn _connection(_: &dyn TvConnection) {}
        fn _connector(_: &dyn TvConnector) {}
        fn _discovery(_: &dyn TvDiscovery) {}
    }
}
