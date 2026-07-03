//! Vendor config for the CEC backend.

use std::str::FromStr;

use linux_cec::PhysicalAddress;
use serde::{Deserialize, Deserializer, Serialize};

/// CEC TV configuration, stored under `[tv.cec]` in the daemon config.
///
/// CEC has no network address, credentials, or pairing secret — a CEC TV
/// needs only the kernel character device that carries its bus and the
/// physical address the adapter should claim on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CecVendorConfig {
    /// Path to the kernel CEC character device (e.g. `/dev/cec0`).
    ///
    /// One adapter exposes one node; a host with several CEC adapters
    /// exposes `/dev/cec0`, `/dev/cec1`, … and each TV pins the one it
    /// is reachable on.
    #[serde(default = "default_device")]
    pub device: String,

    /// Physical address the adapter claims on the bus, in `a.b.c.d` form —
    /// the leading digit is the TV input the adapter is plugged into
    /// (`1.0.0.0` = HDMI 1).
    ///
    /// USB CEC adapters advertise `CEC_CAP_PHYS_ADDR`: the kernel can't
    /// derive their address from a DRM connector's EDID, so the backend
    /// sets this before claiming a logical address — without it the
    /// adapter never joins the bus. GPU DisplayPort-CEC advertises no such
    /// cap (the kernel already assigned an address from the connector), so
    /// the backend leaves it alone and this field is ignored.
    #[serde(
        default = "default_physical_address",
        deserialize_with = "de_physical_address"
    )]
    pub physical_address: PhysicalAddress,

    /// Fulfil screen-blank actions by powering the TV. HDMI-CEC has no
    /// discrete screen-off (the panel can't blank while the TV stays on), so
    /// with this on the backend emulates the `SCREEN` capability via power: a
    /// `screen_off` action (e.g. the stock idle/lock/DPMS profiles) powers
    /// the TV off and `screen_on` powers it back on. Off = the device ignores
    /// screen actions (they read `NotSupported`), for a user who prefers
    /// sub-stock fidelity over power-cycling a slow-to-wake panel. Default on
    /// so the shipped default profiles behave consistently across a fleet.
    #[serde(default = "default_emulate_screen_via_power")]
    pub emulate_screen_via_power: bool,
}

fn default_device() -> String {
    "/dev/cec0".to_string()
}

/// Emulate `SCREEN` via power by default (see the field docs) so a CEC TV
/// honors the stock screen-blank profiles out of the box.
fn default_emulate_screen_via_power() -> bool {
    true
}

/// `1.0.0.0` (HDMI 1) — the common single-adapter wiring.
fn default_physical_address() -> PhysicalAddress {
    PhysicalAddress::from(0x1000)
}

/// Deserialize a physical address, treating an empty string (e.g. a
/// cleared optional pairing-form field) as the default.
fn de_physical_address<'de, D>(deserializer: D) -> Result<PhysicalAddress, D::Error>
where
    D: Deserializer<'de>,
{
    // Accept either the dotted `a.b.c.d`/hex string form or a raw integer
    // (matching linux-cec's own `Deserialize`, so an `UpdateConfig` numeric
    // payload isn't rejected); an empty string — a cleared optional
    // pairing-form field — falls back to the default.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Number(u16),
    }
    match Raw::deserialize(deserializer)? {
        Raw::Text(s) if s.is_empty() => Ok(default_physical_address()),
        Raw::Text(s) => PhysicalAddress::from_str(&s).map_err(serde::de::Error::custom),
        Raw::Number(n) => Ok(PhysicalAddress::from(n)),
    }
}

impl Default for CecVendorConfig {
    fn default() -> Self {
        Self {
            device: default_device(),
            physical_address: default_physical_address(),
            emulate_screen_via_power: default_emulate_screen_via_power(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_physical_address_is_hdmi1() {
        let cfg = CecVendorConfig::default();
        assert_eq!(cfg.physical_address, PhysicalAddress::from(0x1000));
        assert_eq!(cfg.physical_address.to_string(), "1.0.0.0");
    }

    #[test]
    fn emulate_screen_via_power_defaults_on() {
        assert!(CecVendorConfig::default().emulate_screen_via_power);
        // Absent from TOML → still on (the stock profiles work out of the box).
        let cfg: CecVendorConfig = toml::from_str(r#"device = "/dev/cec0""#).unwrap();
        assert!(cfg.emulate_screen_via_power);
        // Explicit opt-out is honored.
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nemulate_screen_via_power = false").unwrap();
        assert!(!cfg.emulate_screen_via_power);
    }

    #[test]
    fn physical_address_round_trips_through_toml() {
        let cfg = CecVendorConfig {
            device: "/dev/cec1".into(),
            physical_address: PhysicalAddress::from(0x2000),
            emulate_screen_via_power: false,
        };
        let value = toml::Value::try_from(&cfg).unwrap();
        let back: CecVendorConfig = value.try_into().unwrap();
        assert_eq!(back.device, "/dev/cec1");
        assert_eq!(back.physical_address, PhysicalAddress::from(0x2000));
        assert!(!back.emulate_screen_via_power, "flag round-trips");
    }

    #[test]
    fn absent_physical_address_uses_default() {
        let cfg: CecVendorConfig = toml::from_str(r#"device = "/dev/cec0""#).unwrap();
        assert_eq!(cfg.physical_address, PhysicalAddress::from(0x1000));
    }

    #[test]
    fn empty_physical_address_uses_default() {
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nphysical_address = \"\"").unwrap();
        assert_eq!(cfg.physical_address, PhysicalAddress::from(0x1000));
    }

    #[test]
    fn parses_dotted_physical_address() {
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nphysical_address = \"2.0.0.0\"").unwrap();
        assert_eq!(cfg.physical_address, PhysicalAddress::from(0x2000));
    }

    #[test]
    fn parses_integer_physical_address() {
        // A raw integer (e.g. an `UpdateConfig` numeric payload) is the
        // packed u16, matching linux-cec's own deserializer.
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nphysical_address = 4096").unwrap();
        assert_eq!(cfg.physical_address, PhysicalAddress::from(0x1000));
    }
}
