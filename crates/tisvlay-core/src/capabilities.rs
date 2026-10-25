//! Capability flags for TV backend feature detection.
//!
//! This module provides the [`Capabilities`] bitflags type that backends use to
//! advertise which features they support. Clients use them to decide which
//! controls a TV offers.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Capabilities a TV backend may support.
    ///
    /// These flags are used to:
    /// - Advertise what a backend can do
    /// - Decide which controls a client offers for a TV
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

        // ----- Transport-family capabilities (orthogonal to features) -----
        //
        // Vendors declare which IP families their transport supports.
        // Consumed by the daemon and GUI to gate user input + UX
        // affordances before any RPC round-trip: the wizard reads
        // capabilities() at form-build time and omits or annotates
        // address fields the vendor can't handle. Derived behaviors
        // (e.g. WoL availability when the only configured address is v6)
        // are *computed*, not stored separately — see Capabilities doc
        // comment for the design rationale.
        //
        // Deliberately not part of STOCK so existing call sites that
        // use STOCK don't silently auto-claim a transport family they
        // haven't verified. Each vendor opts in explicitly.

        /// Transport supports IPv4.
        ///
        /// Vendors over an IP-based transport (WebSocket, raw TCP, HTTP)
        /// that have been verified to work over IPv4 should include this
        /// bit. The daemon uses it to accept v4 addresses in the
        /// pairing form and gate Wake-on-LAN (which is v4-only at the
        /// IEEE protocol level — see `docs/ideas/wol-raw-socket.md`).
        const IPV4 = 0b0001_0000_0000;

        /// Transport supports IPv6.
        ///
        /// Vendors that have been verified to work over IPv6 sockets
        /// (TCP/WebSocket on v6 destinations, NDP-aware probing) should
        /// include this bit. Adding it lets the wizard accept v6
        /// addresses in the manual-entry path and reach the TV over v6
        /// for control. SSDP discovery is a separate concern — vendors
        /// can support v6 control without v6 discovery (LG's expected
        /// shape: WebSocket yes, SSDP no).
        const IPV6 = 0b0010_0000_0000;

        // ----- The TV acting on the PC -----
        //
        // What a connection can tell the PC, for the profiles' PC
        // actions. Out of STOCK like the transport bits: a backend
        // declares one only when its connection delivers it.

        /// The remote's power key, aimed at this PC, reaches it.
        ///
        /// Connections with this capability report the press through
        /// [`TvConnection::subscribe_events`](crate::TvConnection::subscribe_events)
        /// as [`TvEvent::RemotePower`](crate::TvEvent::RemotePower), which
        /// the profiles' `on_remote_power` acts on.
        const REMOTE_POWER = 0b0100_0000_0000;

        /// The remote's keys can reach the desktop, through the kernel's
        /// remote-control input device, when the system profile's
        /// `remote_keys` asks
        /// ([`TvConnection::set_remote_keys`](crate::TvConnection::set_remote_keys)).
        const REMOTE_KEYS = 0b1000_0000_0000;

        /// The stock feature set — the conventional baseline a typical
        /// TV backend is expected to expose. Transport-family bits
        /// (IPV4, IPV6) are deliberately excluded — see their doc
        /// comments above — so vendors using `STOCK` don't silently
        /// claim a transport family they haven't verified.
        ///
        /// Treat this as a curated baseline, not a literal enumeration:
        /// future feature bits (RECORD, EPG, MEDIA_PLAYBACK, …) should
        /// stay out of STOCK for the same opt-in reason. Vendors that
        /// genuinely support a newly-added feature OR it in themselves.
        const STOCK = Self::POWER.bits()
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
    /// The capabilities as lowercase names, in declaration order: their
    /// serde form, and what error messages print.
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
        if self.contains(Self::IPV4) {
            caps.push("ipv4");
        }
        if self.contains(Self::IPV6) {
            caps.push("ipv6");
        }
        if self.contains(Self::REMOTE_POWER) {
            caps.push("remote_power");
        }
        if self.contains(Self::REMOTE_KEYS) {
            caps.push("remote_keys");
        }
        caps
    }

    /// Parse capabilities from a string array.
    ///
    /// This is the inverse of [`to_strings`](Self::to_strings), used to
    /// parse the names a client received on the wire.
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
                "ipv4" => caps |= Self::IPV4,
                "ipv6" => caps |= Self::IPV6,
                "remote_power" => caps |= Self::REMOTE_POWER,
                "remote_keys" => caps |= Self::REMOTE_KEYS,
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
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_strings().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Capabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let strings: Vec<String> = Vec::deserialize(deserializer)?;
        let str_refs: Vec<&str> = strings.iter().map(String::as_str).collect();
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
        let restored = Capabilities::from_strings(&strings);

        assert_eq!(original, restored);
    }

    #[test]
    fn test_capabilities_empty() {
        let caps = Capabilities::empty();
        assert!(caps.to_strings().is_empty());
    }

    #[test]
    fn test_capabilities_stock() {
        let caps = Capabilities::STOCK;
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
    fn test_transport_capabilities_round_trip() {
        // IPV4 / IPV6 are deliberately *not* in STOCK — vendors opt in
        // explicitly per the doc comments on each variant. Verify the
        // bits are independent of the feature-bit STOCK mask.
        assert!(!Capabilities::STOCK.contains(Capabilities::IPV4));
        assert!(!Capabilities::STOCK.contains(Capabilities::IPV6));

        let caps = Capabilities::STOCK | Capabilities::IPV4 | Capabilities::IPV6;
        let strings = caps.to_strings();
        assert!(strings.contains(&"ipv4"));
        assert!(strings.contains(&"ipv6"));
        let restored = Capabilities::from_strings(&strings);
        assert_eq!(caps, restored);
    }

    /// The hand-written names match the lowercased flag names, which is
    /// what a connection's capabilities go out on the wire as.
    #[test]
    fn names_match_the_flag_names() {
        let all = Capabilities::all();
        let flags: Vec<String> = all
            .iter_names()
            .map(|(name, _)| name.to_lowercase())
            .collect();
        assert_eq!(all.to_strings(), flags);
        assert_eq!(
            Capabilities::from_strings(&all.to_strings()),
            all,
            "every name parses back"
        );
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
