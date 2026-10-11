//! The CEC physical address, as the vendor config spells it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A device's position in the HDMI topology: four nibbles, written
/// `a.b.c.d`, where each nibble is an input port one level down from the
/// TV at `0.0.0.0` (`1.0.0.0` = the TV's HDMI 1).
///
/// Parses from the dotted form or four hex digits (optionally `0x`-prefixed)
/// and displays as the dotted form. Serde writes the dotted string and reads
/// either a string or the packed `u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalAddress(u16);

impl PhysicalAddress {
    /// The address from its packed form (`0x1000` = `1.0.0.0`).
    #[must_use]
    pub const fn new(packed: u16) -> Self {
        Self(packed)
    }

    /// The packed form (`1.0.0.0` = `0x1000`).
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The address as linux-cec spells it, for the link layer.
    pub(crate) fn to_linux(self) -> linux_cec::PhysicalAddress {
        linux_cec::PhysicalAddress::from(self.0)
    }
}

impl From<u16> for PhysicalAddress {
    fn from(packed: u16) -> Self {
        Self(packed)
    }
}

impl From<PhysicalAddress> for u16 {
    fn from(address: PhysicalAddress) -> Self {
        address.0
    }
}

impl fmt::Display for PhysicalAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_linux().fmt(f)
    }
}

/// A string that isn't a CEC physical address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsePhysicalAddressError(String);

impl fmt::Display for ParsePhysicalAddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid CEC physical address {:?} (expected a.b.c.d or four hex digits)",
            self.0
        )
    }
}

impl std::error::Error for ParsePhysicalAddressError {}

impl FromStr for PhysicalAddress {
    type Err = ParsePhysicalAddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        linux_cec::PhysicalAddress::from_str(s)
            .map(|address| Self(address.into()))
            .map_err(|_| ParsePhysicalAddressError(s.to_owned()))
    }
}

impl Serialize for PhysicalAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PhysicalAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Packed(u16),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Text(s) => s.parse().map_err(serde::de::Error::custom),
            Raw::Packed(packed) => Ok(Self(packed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_spelling_and_displays_dotted() {
        for s in ["2.0.0.0", "2000", "0x2000"] {
            let address: PhysicalAddress = s.parse().unwrap();
            assert_eq!(address, PhysicalAddress::new(0x2000), "{s}");
            assert_eq!(address.to_string(), "2.0.0.0");
        }
        assert_eq!(PhysicalAddress::new(0x1a3f).to_string(), "1.a.3.f");
    }

    #[test]
    fn rejects_malformed_strings() {
        for s in ["", "1.0.0", "1.0.0.0.0", "10.0.0.0", "g.0.0.0", "0x12345"] {
            assert!(s.parse::<PhysicalAddress>().is_err(), "{s:?} parsed");
        }
    }

    #[test]
    fn link_layer_conversion_keeps_the_packed_form() {
        let address = PhysicalAddress::new(0x4100);
        assert_eq!(u16::from(address.to_linux()), 0x4100);
    }
}
