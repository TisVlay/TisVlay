//! CEC device identity — both directions of the OSD-name exchange.
//!
//! Read side: turns the raw identity the bus answers (the TV's OSD name,
//! vendor OUI) into a cross-vendor [`HardwareId`] (OSD name → model, vendor →
//! manufacturer) plus a canonical vendor id for branding the GUI tile.
//! `linux-cec` exposes only the raw OUI, so the OUI→name lookup lives here.
//!
//! Write side: [`clamp_osd_name`] prepares the name this adapter advertises
//! for *itself* — what the TV shows as the PC's HDMI input label — as the
//! bounded mirror of [`sanitize`].

// External-input profile: this module handles CEC identity replies from
// the TV, so indexing, arithmetic, string slicing and narrowing casts must
// be checked here (docs/build.md, Lint policy).
#![warn(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::cast_possible_truncation
)]

use linux_cec::VendorId;
use tisvlay_core::HardwareId;

use crate::link::CecLink;

/// The identity the CEC connection caches at connect time.
#[derive(Debug, Default, Clone)]
pub(crate) struct CecIdentity {
    /// `manufacturer` (resolved vendor name, else the raw OUI) + `model`
    /// (OSD name); no serial over CEC. `None` when the TV answered nothing.
    pub hardware_id: Option<HardwareId>,
    /// Canonical vendor id for the tile (`lg` / `samsung` / …) when the OUI
    /// resolved; `None` otherwise (the tile falls back to the backend id).
    pub brand: Option<String>,
}

/// Resolve a CEC vendor OUI to `(display name, canonical id)`.
///
/// OUIs are libcec's `CEC_VENDOR_*` table; the canonical id matches the GUI
/// vendor tile's brand keys (`lg` / `samsung` / `sony` get colored tiles,
/// the rest a generic tile with a per-vendor monogram). A best-effort subset
/// of the common TV/AVR makers — an unknown OUI resolves to `None`, and the
/// manufacturer then falls back to the raw OUI.
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

/// Strip control characters (notably the `NUL` that a CEC OSD name field can
/// pad with) and trim surrounding whitespace.
///
/// **Load-bearing for D-Bus:** a string with an embedded `NUL` is an invalid
/// D-Bus message, and emitting one in a `HardwareId`/`Brand` reply makes the
/// bus disconnect the daemon (it loses its well-known name and reads offline
/// to every peer). So every device string is cleaned before it can reach the
/// wire.
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Clamp a user-supplied label to what the CEC OSD-name field can carry, for
/// the *advertise* side — the bounded mirror of [`sanitize`]. Strips control
/// characters (the same NUL/embedded-control hazard `sanitize` guards),
/// truncates to the 14-byte spec limit on a UTF-8 char boundary (never
/// splitting a multi-byte character), and yields `None` when nothing
/// survives — so an all-control label advertises no name rather than an empty
/// one, matching [`query`]'s own drop-if-empty shape.
///
/// **Load-bearing:** `AsyncDevice::set_osd_name` rejects a name over 14 bytes
/// with an error that would fail the whole connection, so the backend must
/// clamp before advertising and never pass a raw label down.
pub(crate) fn clamp_osd_name(name: &str) -> Option<String> {
    /// The CEC OSD-name operand is 14 bytes of (spec-defined) ASCII.
    const MAX_OSD_BYTES: usize = 14;
    let cleaned = sanitize(name);
    // Largest char-boundary prefix that fits in the limit: walk each char's
    // end offset and keep the last one still within 14 bytes.
    let end = cleaned
        .char_indices()
        .map(|(i, c)| i.saturating_add(c.len_utf8()))
        .take_while(|&end| end <= MAX_OSD_BYTES)
        .last()
        .unwrap_or(0);
    cleaned
        .get(..end)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string)
}

/// Query the TV's identity over the link, best-effort. Each query is bounded
/// (a silent TV can't stall connect); a field that fails or comes back empty
/// is simply omitted, and an all-absent result yields `hardware_id: None`.
pub(crate) async fn query(link: &dyn CecLink) -> CecIdentity {
    let model = link
        .query_osd_name()
        .await
        .ok()
        .map(|s| sanitize(&s))
        .filter(|s| !s.is_empty());

    let (name, canonical, raw) = match link.query_vendor_id().await {
        Ok(Some(id)) => {
            let resolved = resolve_vendor(id);
            (
                resolved.map(|(n, _)| n.to_string()),
                resolved.map(|(_, c)| c.to_string()),
                Some(id.to_string()),
            )
        }
        _ => (None, None, None),
    };
    // Make = the resolved name, else the raw OUI (so an unknown vendor still
    // shows *something*); brand keys on the canonical only.
    let manufacturer = name.or(raw);

    let hardware_id = (manufacturer.is_some() || model.is_some()).then_some(HardwareId {
        manufacturer,
        model,
        serial: None,
    });

    CecIdentity {
        hardware_id,
        brand: canonical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_vendor_to_name_and_canonical() {
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
        assert_eq!(sanitize("OLED42C3PU\u{0}\u{0}\u{0}"), "OLED42C3PU");
        assert_eq!(sanitize("  Living Room \t"), "Living Room");
        assert_eq!(sanitize("\u{0}\u{0}"), "");
    }

    #[test]
    fn clamp_osd_name_keeps_short_ascii() {
        assert_eq!(clamp_osd_name("HTPC"), Some("HTPC".to_string()));
        // Exactly 14 bytes is kept whole.
        assert_eq!(
            clamp_osd_name("12345678901234"),
            Some("12345678901234".to_string())
        );
    }

    #[test]
    fn clamp_osd_name_truncates_over_14_bytes() {
        assert_eq!(
            clamp_osd_name("123456789012345"), // 15 ASCII bytes
            Some("12345678901234".to_string()),
        );
    }

    #[test]
    fn clamp_osd_name_strips_control_and_trims_like_sanitize() {
        assert_eq!(
            clamp_osd_name("  HTPC\u{0}\u{0} "),
            Some("HTPC".to_string())
        );
    }

    #[test]
    fn clamp_osd_name_all_control_or_empty_is_none() {
        assert_eq!(clamp_osd_name(""), None);
        assert_eq!(clamp_osd_name("\u{0}\u{0}"), None);
        assert_eq!(clamp_osd_name("   "), None);
    }

    #[test]
    fn clamp_osd_name_truncates_on_char_boundary() {
        // 13 ASCII + "é" (2 bytes) = 15 bytes: 'é' would end at byte 15 > 14,
        // so it is dropped whole rather than split — result is the 13-byte
        // prefix, not an invalid partial code unit.
        let over = "1234567890123é";
        assert_eq!(over.len(), 15);
        assert_eq!(clamp_osd_name(over), Some("1234567890123".to_string()));
        // 12 ASCII + "é" = 14 bytes exactly → kept whole.
        let fits = "123456789012é";
        assert_eq!(fits.len(), 14);
        assert_eq!(clamp_osd_name(fits), Some("123456789012é".to_string()));
    }

    #[tokio::test]
    async fn query_builds_hardware_id_and_brand_sanitized() {
        let link = crate::link::FakeCecLink::new(linux_cec::operand::PowerStatus::On)
            .with_identity("OLED42C3PU\u{0}\u{0}", Some(VendorId([0x00, 0xE0, 0x91])));
        let id = query(&link).await;
        let hw = id.hardware_id.expect("hardware id");
        assert_eq!(hw.model.as_deref(), Some("OLED42C3PU"));
        assert_eq!(hw.manufacturer.as_deref(), Some("LG"));
        assert_eq!(hw.serial, None);
        assert_eq!(id.brand.as_deref(), Some("lg"));
    }

    #[tokio::test]
    async fn query_falls_back_to_raw_oui_when_unknown() {
        let link = crate::link::FakeCecLink::new(linux_cec::operand::PowerStatus::On)
            .with_identity("MyTV", Some(VendorId([0x12, 0x34, 0x56])));
        let id = query(&link).await;
        let hw = id.hardware_id.expect("hardware id");
        assert_eq!(hw.manufacturer.as_deref(), Some("12-34-56"));
        assert_eq!(id.brand, None, "unknown OUI has no canonical brand");
    }

    /// The OSD name comes from a user label; whatever it is, the advertised
    /// name must fit CEC's 14-byte operand and never panic.
    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn clamp_osd_name_never_panics_and_fits(name in any::<String>()) {
                if let Some(clamped) = clamp_osd_name(&name) {
                    prop_assert!(clamped.len() <= 14, "{clamped:?} is over 14 bytes");
                    prop_assert!(sanitize(&name).starts_with(&clamped));
                }
            }
        }
    }
}
