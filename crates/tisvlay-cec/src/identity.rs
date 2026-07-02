//! CEC device-identity resolution.
//!
//! Turns the raw identity the bus answers (OSD name, vendor OUI, CEC
//! version) into a vendor-agnostic [`DeviceIdentity`] for the live INFO
//! group and the vendor tile. `linux-cec` exposes only the raw OUI, so the
//! OUI→name lookup lives here.

use linux_cec::VendorId;
use tisvlay_core::DeviceIdentity;

use crate::link::CecLink;

/// Resolve a CEC vendor OUI to `(display name, canonical id)`.
///
/// OUIs are libcec's `CEC_VENDOR_*` table; the canonical id matches the GUI
/// vendor tile's brand keys (`lg` / `samsung` / `sony` get colored tiles,
/// the rest a generic tile with derived initials). A best-effort subset of
/// the common TV/AVR makers — an unknown OUI resolves to `None` and the
/// INFO row falls back to the raw id.
fn resolve_vendor(id: VendorId) -> Option<(&'static str, &'static str)> {
    let (name, canonical) = match u32::from(id) {
        0x00_E0_91 => ("LG", "lg"),
        0x00_00_F0 => ("Samsung", "samsung"),
        0x08_00_46 => ("Sony", "sony"),
        0x00_80_45 => ("Panasonic", "panasonic"),
        0x00_90_3E => ("Philips", "philips"),
        0x08_00_1F | 0x53_48_50 => ("Sharp", "sharp"),
        0x00_00_39 | 0x00_0C_E7 => ("Toshiba", "toshiba"),
        0x6B_74_6D => ("Vizio", "vizio"),
        0x00_E0_36 => ("Pioneer", "pioneer"),
        0x00_09_B0 => ("Onkyo", "onkyo"),
        0x00_A0_DE => ("Yamaha", "yamaha"),
        0x00_05_CD => ("Denon", "denon"),
        0x00_06_78 => ("Marantz", "marantz"),
        0x00_10_FA => ("Apple", "apple"),
        0x00_1A_11 => ("Google", "google"),
        0x00_D0_D5 => ("Grundig", "grundig"),
        0x00_09_82 => ("Loewe", "loewe"),
        0x80_65_E9 => ("BenQ", "benq"),
        _ => return None,
    };
    Some((name, canonical))
}

/// Strip control characters (notably the `NUL` that CEC's fixed-width OSD
/// name field pads with) and trim surrounding whitespace.
///
/// **Load-bearing for D-Bus:** a string with an embedded `NUL` is an invalid
/// D-Bus message, and emitting one in the `VendorInfo`/`Brand`
/// `PropertiesChanged` signal makes the bus disconnect the daemon (it loses
/// its well-known name and reads offline to every peer). So every device
/// string is cleaned before it can reach the wire.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Query the device's identity over the link, best-effort: each field that
/// fails to answer (or comes back empty) is simply omitted. An all-absent
/// result is `is_empty()` and the state merge drops it.
pub(crate) async fn query(link: &dyn CecLink) -> DeviceIdentity {
    let osd_name = link
        .query_osd_name()
        .await
        .ok()
        .map(|s| sanitize(&s))
        .filter(|s| !s.is_empty());

    let (vendor_raw, vendor_name, vendor_canonical) = match link.query_vendor_id().await {
        Ok(Some(id)) => {
            let resolved = resolve_vendor(id);
            (
                Some(id.to_string()),
                resolved.map(|(name, _)| name.to_string()),
                resolved.map(|(_, canonical)| canonical.to_string()),
            )
        }
        _ => (None, None, None),
    };

    let cec_version = link
        .query_cec_version()
        .await
        .ok()
        .filter(|s| !s.is_empty() && s != "unknown");

    DeviceIdentity {
        osd_name,
        vendor_raw,
        vendor_name,
        vendor_canonical,
        cec_version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_vendor_to_name_and_canonical() {
        // 00-e0-91 is LG (from the C3 cec-ctl dump).
        let lg = VendorId([0x00, 0xE0, 0x91]);
        assert_eq!(resolve_vendor(lg), Some(("LG", "lg")));
        assert_eq!(lg.to_string(), "00-e0-91");
    }

    #[test]
    fn unknown_vendor_resolves_to_none() {
        assert_eq!(resolve_vendor(VendorId([0x12, 0x34, 0x56])), None);
    }

    #[test]
    fn sanitize_strips_nul_and_control_and_trims() {
        // The exact shape that crashes the daemon on D-Bus: a name with an
        // embedded NUL. Must come out clean (and empty if only padding).
        assert_eq!(sanitize("OLED42C3PU\u{0}\u{0}\u{0}"), "OLED42C3PU");
        assert_eq!(sanitize("  Living Room \t"), "Living Room");
        assert_eq!(sanitize("\u{0}\u{0}"), "");
    }

    #[tokio::test]
    async fn query_sanitizes_the_osd_name() {
        // A fake link whose OSD name carries NUL padding — `query` must strip
        // it so the string is safe to put on D-Bus.
        let link = crate::link::FakeCecLink::new(linux_cec::operand::PowerStatus::On)
            .with_identity(
                "OLED42C3PU\u{0}\u{0}",
                Some(VendorId([0x00, 0xE0, 0x91])),
                "1.4",
            );
        let id = query(&link).await;
        assert_eq!(id.osd_name.as_deref(), Some("OLED42C3PU"));
        assert_eq!(id.vendor_canonical.as_deref(), Some("lg"));
    }
}
