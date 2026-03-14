//! Wake-on-LAN implementation.
//!
//! Sends "magic packets" to wake devices from standby mode.

use crate::error::{Error, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

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

        if s.len() != 12 {
            return Err(Error::InvalidMacAddress(format!(
                "Expected 12 hex digits, got {}",
                s.len()
            )));
        }

        let bytes: std::result::Result<Vec<u8>, _> = (0..6)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
            .collect();

        let bytes = bytes.map_err(|e| Error::InvalidMacAddress(e.to_string()))?;

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&bytes);
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
    for i in 0..16 {
        let offset = 6 + i * 6;
        packet[offset..offset + 6].copy_from_slice(mac.bytes());
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

/// Send Wake-on-LAN to a specific IP address (in addition to broadcast).
///
/// Some network configurations work better with directed WoL.
pub fn wake_to_ip(mac: &MacAddress, ip: Ipv4Addr) -> Result<()> {
    let packet = build_magic_packet(mac);

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| Error::WakeOnLan(format!("Failed to create socket: {}", e)))?;

    let dest = SocketAddr::V4(SocketAddrV4::new(ip, 9));

    socket
        .send_to(&packet, &dest.into())
        .map_err(|e| Error::WakeOnLan(format!("Failed to send packet: {}", e)))?;

    tracing::debug!(mac = %mac, ip = %ip, "Sent directed Wake-on-LAN packet");

    Ok(())
}

/// Send Wake-on-LAN using multiple methods for reliability.
///
/// Sends to:
/// 1. Global broadcast (255.255.255.255)
/// 2. Directed to the TV's IP address
pub fn wake_reliable(mac: &MacAddress, ip: Option<Ipv4Addr>) -> Result<()> {
    // Send to global broadcast
    wake(mac)?;

    // Also send directly to IP if known
    if let Some(ip) = ip {
        if let Err(e) = wake_to_ip(mac, ip) {
            tracing::warn!("Directed WoL failed (broadcast may still work): {}", e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
