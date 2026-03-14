//! Capability flags for TV backend feature detection.
//!
//! This module provides the [`Capabilities`] bitflags type that backends use to
//! advertise which features they support. The D-Bus interface uses these capabilities
//! to determine which feature interfaces to expose for each TV.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Capabilities a TV backend may support.
    ///
    /// These flags are used to:
    /// - Advertise what a backend can do
    /// - Determine which D-Bus feature interfaces to expose
    /// - Gate operations at runtime to prevent unsupported calls
    ///
    /// # Example
    ///
    /// ```
    /// use tisvlay_core::Capabilities;
    ///
    /// let caps = Capabilities::POWER | Capabilities::SCREEN | Capabilities::VOLUME;
    ///
    /// assert!(caps.contains(Capabilities::POWER));
    /// assert!(!caps.contains(Capabilities::BACKLIGHT));
    ///
    /// let strings = caps.to_strings();
    /// assert!(strings.contains(&"power"));
    /// assert!(strings.contains(&"screen"));
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Capabilities: u32 {
        /// Power on/off control.
        ///
        /// Backends with this capability can turn the TV on (typically via WoL)
        /// and off (standby).
        const POWER = 0b0000_0001;

        /// Screen blanking control (OLED-safe).
        ///
        /// Backends with this capability can blank/unblank the screen without
        /// fully powering off the TV. This is safer for OLED panels.
        const SCREEN = 0b0000_0010;

        /// Volume control.
        ///
        /// Backends with this capability can get and set the volume level.
        const VOLUME = 0b0000_0100;

        /// Mute control.
        ///
        /// Backends with this capability can mute/unmute audio.
        /// Note: This is separate from VOLUME as some backends may support
        /// one without the other.
        const MUTE = 0b0000_1000;

        /// Input/source switching.
        ///
        /// Backends with this capability can switch between inputs (HDMI, etc.)
        /// and list available inputs.
        const INPUT = 0b0001_0000;

        /// Backlight/brightness control.
        ///
        /// Backends with this capability can control the backlight level.
        /// For OLED TVs, this is the pixel brightness.
        const BACKLIGHT = 0b0010_0000;

        /// Wake-on-LAN support.
        ///
        /// Backends with this capability can wake the TV from standby using
        /// Wake-on-LAN magic packets. Requires MAC address in configuration.
        const WOL = 0b0100_0000;

        /// Network discovery support.
        ///
        /// Backends with this capability can discover TVs on the local network
        /// (via SSDP, mDNS, or vendor-specific protocols).
        const DISCOVERY = 0b1000_0000;

        /// All capabilities combined.
        const ALL = Self::POWER.bits()
            | Self::SCREEN.bits()
            | Self::VOLUME.bits()
            | Self::MUTE.bits()
            | Self::INPUT.bits()
            | Self::BACKLIGHT.bits()
            | Self::WOL.bits()
            | Self::DISCOVERY.bits();
    }
}

impl Capabilities {
    /// Convert capabilities to a D-Bus-friendly string array.
    ///
    /// This is used when exposing capabilities via D-Bus properties.
    ///
    /// # Example
    ///
    /// ```
    /// use tisvlay_core::Capabilities;
    ///
    /// let caps = Capabilities::POWER | Capabilities::VOLUME;
    /// let strings = caps.to_strings();
    ///
    /// assert_eq!(strings, vec!["power", "volume"]);
    /// ```
    pub fn to_strings(&self) -> Vec<&'static str> {
        let mut caps = Vec::new();
        if self.contains(Self::POWER) {
            caps.push("power");
        }
        if self.contains(Self::SCREEN) {
            caps.push("screen");
        }
        if self.contains(Self::VOLUME) {
            caps.push("volume");
        }
        if self.contains(Self::MUTE) {
            caps.push("mute");
        }
        if self.contains(Self::INPUT) {
            caps.push("input");
        }
        if self.contains(Self::BACKLIGHT) {
            caps.push("backlight");
        }
        if self.contains(Self::WOL) {
            caps.push("wol");
        }
        if self.contains(Self::DISCOVERY) {
            caps.push("discovery");
        }
        caps
    }

    /// Parse capabilities from a string array.
    ///
    /// This is the inverse of [`to_strings`](Self::to_strings), useful for
    /// parsing capabilities received over D-Bus.
    ///
    /// Unknown capability strings are silently ignored.
    pub fn from_strings(strings: &[&str]) -> Self {
        let mut caps = Self::empty();
        for s in strings {
            match *s {
                "power" => caps |= Self::POWER,
                "screen" => caps |= Self::SCREEN,
                "volume" => caps |= Self::VOLUME,
                "mute" => caps |= Self::MUTE,
                "input" => caps |= Self::INPUT,
                "backlight" => caps |= Self::BACKLIGHT,
                "wol" => caps |= Self::WOL,
                "discovery" => caps |= Self::DISCOVERY,
                _ => {} // Ignore unknown capabilities
            }
        }
        caps
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::empty()
    }
}

impl Serialize for Capabilities {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_strings().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Capabilities {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let strings: Vec<String> = Vec::deserialize(deserializer)?;
        let str_refs: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
        Ok(Self::from_strings(&str_refs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capabilities_to_strings() {
        let caps = Capabilities::POWER | Capabilities::SCREEN | Capabilities::VOLUME;
        let strings = caps.to_strings();

        assert_eq!(strings.len(), 3);
        assert!(strings.contains(&"power"));
        assert!(strings.contains(&"screen"));
        assert!(strings.contains(&"volume"));
    }

    #[test]
    fn test_capabilities_from_strings() {
        let caps = Capabilities::from_strings(&["power", "volume", "unknown"]);

        assert!(caps.contains(Capabilities::POWER));
        assert!(caps.contains(Capabilities::VOLUME));
        assert!(!caps.contains(Capabilities::SCREEN));
    }

    #[test]
    fn test_capabilities_roundtrip() {
        let original = Capabilities::POWER | Capabilities::MUTE | Capabilities::INPUT;
        let strings = original.to_strings();
        let str_refs: Vec<&str> = strings.iter().copied().collect();
        let restored = Capabilities::from_strings(&str_refs);

        assert_eq!(original, restored);
    }

    #[test]
    fn test_capabilities_empty() {
        let caps = Capabilities::empty();
        assert!(caps.to_strings().is_empty());
    }

    #[test]
    fn test_capabilities_all() {
        let caps = Capabilities::ALL;
        let strings = caps.to_strings();

        assert_eq!(strings.len(), 8);
        assert!(strings.contains(&"power"));
        assert!(strings.contains(&"screen"));
        assert!(strings.contains(&"volume"));
        assert!(strings.contains(&"mute"));
        assert!(strings.contains(&"input"));
        assert!(strings.contains(&"backlight"));
        assert!(strings.contains(&"wol"));
        assert!(strings.contains(&"discovery"));
    }

    #[test]
    fn test_capabilities_serde() {
        let caps = Capabilities::POWER | Capabilities::VOLUME;
        let json = serde_json::to_string(&caps).unwrap();
        assert_eq!(json, r#"["power","volume"]"#);

        let restored: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, restored);
    }
}
