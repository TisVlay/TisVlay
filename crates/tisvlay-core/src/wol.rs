//! Wake-on-LAN implementation.
//!
//! Sends "magic packets" to wake devices from standby mode.

// External-input profile: this module handles MAC strings from config and
// from the TV, so indexing, arithmetic, string slicing and narrowing casts
// must be checked here (docs/build.md, Lint policy).
#![warn(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::cast_possible_truncation
)]

use crate::error::{Error, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// The cross-vendor config key carrying a TV's Wake-on-LAN MAC. Vendors
/// whose wake path needs a MAC store it under this key, which lets the
/// daemon fill it generically (manual-entry ARP enrichment, the
/// device-reported link MAC at pair time).
pub const MAC_CONFIG_KEY: &str = "mac";

/// MAC address representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    /// Parse a MAC address from a string.
    ///
    /// Accepts formats like:
    /// - `AA:BB:CC:DD:EE:FF`
    /// - `AA-BB-CC-DD-EE-FF`
    /// - `AABBCCDDEEFF`
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.replace(['-', ':'], "");

        // Check the digits before slicing: the byte-indexed slices below
        // would panic on a multi-byte character.
        if s.len() != 12 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidMacAddress(format!(
                "Expected 12 hex digits, got {s:?}"
            )));
        }

        let mut mac = [0u8; 6];
        for (byte, pair) in mac.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
            let pair =
                std::str::from_utf8(pair).map_err(|e| Error::InvalidMacAddress(e.to_string()))?;
            *byte = u8::from_str_radix(pair, 16)
                .map_err(|e| Error::InvalidMacAddress(e.to_string()))?;
        }
        Ok(Self(mac))
    }

    /// Get the raw bytes.
    pub fn bytes(&self) -> &[u8; 6] {
        &self.0
    }
}

impl std::fmt::Display for MacAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

/// Build a Wake-on-LAN magic packet.
///
/// The magic packet consists of:
/// - 6 bytes of 0xFF
/// - 16 repetitions of the target MAC address
fn build_magic_packet(mac: &MacAddress) -> [u8; 102] {
    let mut packet = [0u8; 102];

    // 6 bytes of 0xFF
    packet[..6].fill(0xFF);

    // 16 repetitions of MAC address
    for repetition in packet[6..].as_chunks_mut::<6>().0 {
        repetition.copy_from_slice(mac.bytes());
    }

    packet
}

/// Send a Wake-on-LAN packet to wake a TV.
///
/// Sends the magic packet to the broadcast address (255.255.255.255:9).
pub fn wake(mac: &MacAddress) -> Result<()> {
    wake_with_options(mac, WakeOptions::default())
}

/// Options for Wake-on-LAN.
#[derive(Debug, Clone)]
pub struct WakeOptions {
    /// Broadcast address to send to.
    pub broadcast_addr: Ipv4Addr,
    /// Port to send to (default: 9).
    pub port: u16,
}

impl Default for WakeOptions {
    fn default() -> Self {
        Self {
            broadcast_addr: Ipv4Addr::BROADCAST,
            port: 9,
        }
    }
}

impl WakeOptions {
    /// Use a specific broadcast address (e.g., subnet broadcast).
    pub fn with_broadcast(mut self, addr: Ipv4Addr) -> Self {
        self.broadcast_addr = addr;
        self
    }

    /// Use a specific port.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// Send a Wake-on-LAN packet with custom options.
pub fn wake_with_options(mac: &MacAddress, options: WakeOptions) -> Result<()> {
    let packet = build_magic_packet(mac);

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| Error::WakeOnLan(format!("Failed to create socket: {}", e)))?;

    socket
        .set_broadcast(true)
        .map_err(|e| Error::WakeOnLan(format!("Failed to enable broadcast: {}", e)))?;

    let dest = SocketAddr::V4(SocketAddrV4::new(options.broadcast_addr, options.port));

    socket
        .send_to(&packet, &dest.into())
        .map_err(|e| Error::WakeOnLan(format!("Failed to send packet: {}", e)))?;

    tracing::debug!(
        mac = %mac,
        broadcast = %options.broadcast_addr,
        port = options.port,
        "Sent Wake-on-LAN packet"
    );

    Ok(())
}

/// Send a Wake-on-LAN packet, retrying while the local NIC recovers.
///
/// The first attempt can fail with "network unreachable" right after
/// system resume, before the desktop's interface is back up. Retries
/// the send up to `max_attempts` times with a short backoff, returning
/// the last send error (or a generic failure) if every attempt fails.
///
/// The `max_attempts` cap is the caller's knob; the backoff curve is a
/// fixed NIC-recovery policy (not vendor-specific), so it stays
/// internal. Callers log their own success line — this helper has only
/// the MAC, not the device's IP.
pub async fn wake_with_retry(mac: &MacAddress, max_attempts: u32) -> Result<()> {
    let mut last_err = None;
    for attempt in 1..=max_attempts {
        match wake(mac) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(mac = %mac, attempt, error = %e, "WoL send failed, retrying");
                last_err = Some(e);
                let delay = if attempt <= 2 { 2000 } else { 3000 };
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::WakeOnLan("WoL failed after retries".into())))
}

/// Send a single Wake-on-LAN packet, logging (not returning) any error.
///
/// For best-effort resends inside a connect-retry loop, where a lost
/// packet is retried by the next iteration anyway, so one failed send
/// shouldn't abort the loop.
pub fn wake_best_effort(mac: &MacAddress) {
    if let Err(e) = wake(mac) {
        // Debug, not warn: this fires every loop iteration against a
        // dead NIC (a ~30 s wake dial is dozens of lines), and the
        // dial's real outcome surfaces through ConnectFailed anyway.
        tracing::debug!(mac = %mac, error = %e, "WoL resend failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parse_rejects_non_ascii_without_panicking() {
        // Six two-byte characters: 12 bytes, so it passes a length check,
        // but the byte-indexed slicing would split a character.
        assert!(MacAddress::parse("éééééé").is_err());
        assert!(MacAddress::parse("éé:éé:éé:éé:éé:éé").is_err());
        assert!(MacAddress::parse("GG:BB:CC:DD:EE:FF").is_err());
    }

    #[test]
    fn test_parse_mac_colon() {
        let mac = MacAddress::parse("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(mac.0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_mac_dash() {
        let mac = MacAddress::parse("AA-BB-CC-DD-EE-FF").unwrap();
        assert_eq!(mac.0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_mac_plain() {
        let mac = MacAddress::parse("AABBCCDDEEFF").unwrap();
        assert_eq!(mac.0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_mac_lowercase() {
        let mac = MacAddress::parse("aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(mac.0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_mac_invalid() {
        assert!(MacAddress::parse("invalid").is_err());
        assert!(MacAddress::parse("AA:BB:CC:DD:EE").is_err());
        assert!(MacAddress::parse("AA:BB:CC:DD:EE:FF:GG").is_err());
    }

    #[test]
    fn test_mac_display() {
        let mac = MacAddress::parse("aabbccddeeff").unwrap();
        assert_eq!(mac.to_string(), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn test_magic_packet() {
        let mac = MacAddress::parse("AA:BB:CC:DD:EE:FF").unwrap();
        let packet = build_magic_packet(&mac);

        // First 6 bytes should be 0xFF
        assert!(packet[..6].iter().all(|&b| b == 0xFF));

        // Remaining 96 bytes should be 16 repetitions of MAC
        for i in 0..16 {
            let offset = 6 + i * 6;
            assert_eq!(&packet[offset..offset + 6], mac.bytes());
        }
    }

    #[tokio::test]
    async fn wake_with_retry_exhausts_to_error() {
        let mac = MacAddress::parse("AA:BB:CC:DD:EE:FF").unwrap();
        // A zero-attempt cap runs an empty `1..=0` loop, so nothing is
        // sent and the call falls through to the generic failure. This
        // is the only network-free branch of the loop — it exercises
        // the exhaustion fallback without a socket or a backoff sleep.
        let err = wake_with_retry(&mac, 0).await.unwrap_err();
        assert!(matches!(err, Error::WakeOnLan(_)));
    }

    proptest! {
        /// Parsing any 6 arbitrary bytes via their Display representation
        /// roundtrips back to the original bytes.
        #[test]
        fn mac_parse_display_roundtrip(bytes in any::<[u8; 6]>()) {
            let original = MacAddress(bytes);
            let parsed = MacAddress::parse(&original.to_string()).unwrap();
            prop_assert_eq!(parsed, original);
        }

        /// MacAddress::parse accepts the three valid separator styles
        /// (colon, dash, plain) for any 6 bytes — all should yield the
        /// same MacAddress.
        #[test]
        fn mac_parse_all_separator_styles(bytes in any::<[u8; 6]>()) {
            let colon = format!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
            );
            let dash = colon.replace(':', "-");
            let plain = colon.replace(':', "");

            let from_colon = MacAddress::parse(&colon).unwrap();
            let from_dash = MacAddress::parse(&dash).unwrap();
            let from_plain = MacAddress::parse(&plain).unwrap();

            prop_assert_eq!(from_colon, MacAddress(bytes));
            prop_assert_eq!(from_dash, MacAddress(bytes));
            prop_assert_eq!(from_plain, MacAddress(bytes));
        }

        /// Lowercase hex parses to the same MAC as uppercase.
        #[test]
        fn mac_parse_case_insensitive(bytes in any::<[u8; 6]>()) {
            let upper = MacAddress(bytes).to_string();
            let lower = upper.to_lowercase();
            prop_assert_eq!(
                MacAddress::parse(&upper).unwrap(),
                MacAddress::parse(&lower).unwrap(),
            );
        }

        /// Inputs whose total hex-digit length isn't 12 always fail to parse.
        #[test]
        fn mac_parse_rejects_wrong_length(
            s in "[0-9A-Fa-f]{0,11}|[0-9A-Fa-f]{13,30}"
        ) {
            prop_assert!(MacAddress::parse(&s).is_err());
        }
    }
}
