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

    /// Advertise our OSD name on the bus at all. On (the default), the TV
    /// labels the PC's HDMI input with [`Self::osd_name`]; off, we configure
    /// no name, the kernel stays silent on `<Give OSD Name>`, and the input
    /// keeps the TV's own default label. The off state is a first-class flag
    /// (not an empty name) so it survives the schema form on every path.
    #[serde(default = "default_advertise_osd_name")]
    pub advertise_osd_name: bool,

    /// OSD name this adapter advertises for *itself* on the bus — the label
    /// the TV shows for the PC's HDMI input, the way a Chromecast names its
    /// input "Chromecast". Distinct from the TV's own OSD name that
    /// discovery/identity *reads* (`identity::query`) to fill Make/Model:
    /// this is the reverse direction, what we answer when the TV asks *us*
    /// for a name.
    ///
    /// Defaults to `DEFAULT_OSD_NAME` (`"TisVlay"`); disable advertising via
    /// [`Self::advertise_osd_name`], not by emptying the name. Normalized at
    /// parse (`de_osd_name`): control characters stripped, clamped to the
    /// 14-byte CEC field on a char boundary, and an empty result falls back
    /// to the default — mirroring how an empty `physical_address` falls back
    /// to its default, so "empty" uniformly means "absent" across the config.
    #[serde(default = "default_osd_name", deserialize_with = "de_osd_name")]
    pub osd_name: String,
}

/// The OSD name advertised out of the box — the single source for both the
/// serde default and the pairing-schema default, so the form prefill and the
/// parsed config can never drift.
pub(crate) const DEFAULT_OSD_NAME: &str = "TisVlay";

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

/// Advertise by default, so a CEC TV labels the PC's input out of the box.
fn default_advertise_osd_name() -> bool {
    true
}

fn default_osd_name() -> String {
    DEFAULT_OSD_NAME.to_string()
}

/// Deserialize + normalize the advertised OSD name: strip control
/// characters, clamp to the 14-byte CEC field on a char boundary, and treat
/// an empty result as "absent" (fall back to the default). Normalizing at
/// the boundary keeps the stored, displayed, and advertised names identical.
fn de_osd_name<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(crate::identity::clamp_osd_name(&raw).unwrap_or_else(default_osd_name))
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
            advertise_osd_name: default_advertise_osd_name(),
            osd_name: default_osd_name(),
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
            advertise_osd_name: false,
            osd_name: "Den PC".into(),
        };
        let value = toml::Value::try_from(&cfg).unwrap();
        let back: CecVendorConfig = value.try_into().unwrap();
        assert_eq!(back.device, "/dev/cec1");
        assert_eq!(back.physical_address, PhysicalAddress::from(0x2000));
        assert!(!back.emulate_screen_via_power, "flag round-trips");
        assert!(!back.advertise_osd_name, "advertise flag round-trips");
        assert_eq!(back.osd_name, "Den PC", "osd_name round-trips");
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

    #[test]
    fn osd_name_defaults_on_and_to_the_project_name() {
        // Absent in TOML → advertising on, project name — a CEC TV labels
        // the PC's input out of the box.
        let cfg: CecVendorConfig = toml::from_str(r#"device = "/dev/cec0""#).unwrap();
        assert!(cfg.advertise_osd_name);
        assert_eq!(cfg.osd_name, DEFAULT_OSD_NAME);
        // The off switch is the flag, not an emptied name.
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nadvertise_osd_name = false").unwrap();
        assert!(!cfg.advertise_osd_name);
    }

    #[test]
    fn osd_name_normalizes_at_parse() {
        // Empty ≡ absent ≡ default — the same rule an empty
        // physical_address follows, so "clear a field" is uniform.
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nosd_name = \"\"").unwrap();
        assert_eq!(cfg.osd_name, DEFAULT_OSD_NAME);
        // Over-long names clamp at the boundary, so the stored, displayed,
        // and advertised names agree.
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nosd_name = \"123456789012345\"").unwrap();
        assert_eq!(cfg.osd_name, "12345678901234");
        // Control characters strip; surviving text is kept.
        let cfg: CecVendorConfig =
            toml::from_str("device = \"/dev/cec0\"\nosd_name = \" HTPC\\u0000 \"").unwrap();
        assert_eq!(cfg.osd_name, "HTPC");
    }
}
