//! LG WebOS TV discovery via SSDP.
//!
//! Sends M-SEARCH bursts for the LG-specific and standard UPnP targets,
//! then filters and parses responses with LG-specific heuristics. The
//! SSDP transport itself lives in the vendor-neutral [`tisvlay_ssdp`]
//! crate.

use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tisvlay_core::{DiscoveredTv, HardwareId, Result, TvDiscovery};
use tisvlay_ssdp::{SharedSearcher, SsdpResponse};

/// Search targets that LG WebOS TVs respond to.
///
/// Different LG models / firmware versions respond to different
/// subsets: the LG-specific URN catches modern WebOS TVs; the
/// generic `MediaRenderer:1` catches most; `upnp:rootdevice` is the
/// fallback that catches whatever's left advertising as a UPnP root.
/// All three are sent on a single shared socket, so the cost is one
/// extra UDP datagram per extra target — not a multiplied timeout.
const LG_TARGETS: &[&str] = &[
    "urn:lge-com:service:webostv:1",
    "urn:schemas-upnp-org:device:MediaRenderer:1",
    "upnp:rootdevice",
];

/// Filter, parse, and de-duplicate a stream of SSDP responses into LG
/// TVs. Not finding any is not an error — the result is just empty.
///
/// De-dup is by IP address (each TV answers from several UPnP services
/// with different UUIDs), preferring entries that carry a real DLNA
/// device name over the `LG TV (ip)` fallback.
async fn collect_lg_tvs(stream: impl Stream<Item = SsdpResponse>) -> Vec<DiscoveredTv> {
    tokio::pin!(stream);
    let mut tvs: HashMap<String, DiscoveredTv> = HashMap::new();

    while let Some(resp) = stream.next().await {
        if !is_lg_tv(&resp) {
            continue;
        }
        let Some(tv) = parse_response(&resp) else {
            continue;
        };
        tracing::debug!(id = %tv.id, name = %tv.name, address = %tv.address, "Discovered LG TV");
        let dominated = tvs
            .get(&tv.address)
            .is_some_and(|existing| has_dlna_name(existing) && !has_dlna_name(&tv));
        if !dominated {
            tvs.insert(tv.address.clone(), tv);
        }
    }

    tracing::info!(count = tvs.len(), "LG SSDP discovery complete");
    tvs.into_values().collect()
}

/// Check if a discovered TV has a real DLNA device name (vs fallback).
fn has_dlna_name(tv: &DiscoveredTv) -> bool {
    tv.name.starts_with("[LG]")
}

/// Check if an SSDP response is from an LG WebOS TV.
fn is_lg_tv(resp: &SsdpResponse) -> bool {
    // LG-specific DLNA header (most reliable)
    resp.extra_header("DLNADeviceName.lge.com").is_some()
        // Server string contains LGE or WebOS
        || resp.server().contains("LGE")
        || resp.server().contains("WebOS")
        || resp.server().contains("webOS")
        // USN contains LG-specific URN
        || resp.usn().contains("lge-com")
}

/// Parse an SSDP response into a DiscoveredTv.
fn parse_response(resp: &SsdpResponse) -> Option<DiscoveredTv> {
    let id = extract_uuid_from_usn(resp.usn())?;
    let location = resp.location();
    let ip = parse_ip_from_url(location)?;

    let name = resp
        .extra_header("DLNADeviceName.lge.com")
        .and_then(|s| urlencoding::decode(s).ok())
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|| format!("LG TV ({ip})"));

    // LG's EDID PNPID is "GSM" — always fill it so downstream matching
    // (e.g. EDID cross-reference for display matching) has a
    // manufacturer to work with. Model is best-effort: parsed from the
    // DLNA device name when it's in the "[LG] webOS TV <MODEL>" form.
    // Serial isn't available over SSDP — the backend fills it after
    // connecting.
    let hardware_id = Some(HardwareId::new(
        Some(LG_PNPID),
        parse_model_from_dlna_name(&name).map(str::to_string),
        None::<String>,
    ));

    Some(DiscoveredTv {
        id,
        name,
        address: ip,
        hardware_id,
        vendor: crate::VENDOR_ID.to_string(),
        location: location.to_string(),
        // `source` tags how the TV was found (SSDP for LG's network scan)
        // so the wizard can show a discovery-source chip.
        vendor_data: HashMap::from([("source".to_string(), "ssdp".to_string())]),
    })
}

/// LG's EDID PNPID manufacturer code.
const LG_PNPID: &str = "GSM";

/// Extract the model string from a DLNA device name like
/// `"[LG] webOS TV OLED65G4SUB"`. Returns `None` when the name
/// doesn't follow that convention.
fn parse_model_from_dlna_name(name: &str) -> Option<&str> {
    // Accept either "[LG] webOS TV <MODEL>" or just anything after
    // "[LG]" — the model is whatever comes after the last space.
    name.strip_prefix("[LG]")
        .and_then(|rest| rest.trim().rsplit_once(' '))
        .map(|(_, model)| model)
        .filter(|m| !m.is_empty())
}

/// Extract UUID from USN header.
///
/// USN format: `uuid:xxx::urn:...` or just `uuid:xxx`
fn extract_uuid_from_usn(usn: &str) -> Option<String> {
    usn.strip_prefix("uuid:")
        .map(|s| s.split("::").next().unwrap_or(s).to_string())
}

/// Parse IP address from a URL.
fn parse_ip_from_url(location: &str) -> Option<String> {
    url::Url::parse(location)
        .ok()?
        .host_str()
        .map(ToString::to_string)
}

/// LG-specific network discovery — the [`TvDiscovery`] implementation.
///
/// Wraps a shared SSDP socket ([`SharedSearcher`]) so overlapping
/// discovery sessions reuse one bound, firewall-targetable source port
/// instead of each binding their own. LG search targets and response
/// filtering are layered on top.
#[derive(Debug)]
pub struct LgDiscovery {
    searcher: Arc<SharedSearcher>,
}

impl LgDiscovery {
    /// Create LG discovery with `bind_port` as the SSDP source port
    /// (`0` lets the OS pick an ephemeral one). The socket binds lazily
    /// on the first `discover`. See `docs/firewall.md`.
    pub fn new(bind_port: u16) -> Self {
        let targets = LG_TARGETS.iter().map(ToString::to_string).collect();
        Self {
            searcher: Arc::new(SharedSearcher::new(bind_port, targets)),
        }
    }
}

impl TvDiscovery for LgDiscovery {
    async fn discover(&self, timeout: Duration) -> Result<Vec<DiscoveredTv>> {
        tracing::debug!(
            timeout_ms = timeout.as_millis(),
            "Starting LG SSDP discovery"
        );
        let stream = match self.searcher.search(timeout).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "SSDP search setup failed");
                return Ok(Vec::new());
            }
        };
        Ok(collect_lg_tvs(stream).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_uuid_from_usn() {
        // Full USN with service type
        let usn = "uuid:12345678-1234-1234-1234-123456789abc::urn:lge-com:service:webostv:1";
        assert_eq!(
            extract_uuid_from_usn(usn),
            Some("12345678-1234-1234-1234-123456789abc".to_string())
        );

        // UUID only
        let usn_only = "uuid:abcd-1234";
        assert_eq!(
            extract_uuid_from_usn(usn_only),
            Some("abcd-1234".to_string())
        );

        // Invalid format
        assert_eq!(extract_uuid_from_usn("not-a-uuid"), None);
    }

    #[test]
    fn test_has_dlna_name() {
        let with_dlna = DiscoveredTv {
            id: "uuid".to_string(),
            name: "[LG] webOS TV OLED65G4SUB".to_string(),
            address: "192.168.1.170".to_string(),
            hardware_id: None,
            vendor: "lg".to_string(),
            location: String::new(),
            vendor_data: HashMap::new(),
        };
        assert!(has_dlna_name(&with_dlna));

        let fallback = DiscoveredTv {
            name: "LG TV (192.168.1.170)".to_string(),
            ..with_dlna
        };
        assert!(!has_dlna_name(&fallback));
    }

    #[test]
    fn test_parse_model_from_dlna_name() {
        assert_eq!(
            parse_model_from_dlna_name("[LG] webOS TV OLED65G4SUB"),
            Some("OLED65G4SUB")
        );
        assert_eq!(
            parse_model_from_dlna_name("[LG] webOS TV OLED42C3PUA"),
            Some("OLED42C3PUA")
        );
        // Fallback name (no "[LG]" prefix) — no model extracted.
        assert_eq!(parse_model_from_dlna_name("LG TV (192.168.1.170)"), None);
        // Bracket but nothing after it — no model.
        assert_eq!(parse_model_from_dlna_name("[LG]"), None);
    }

    #[test]
    fn test_parse_ip_from_url() {
        assert_eq!(
            parse_ip_from_url("http://192.168.1.100:1925/"),
            Some("192.168.1.100".to_string())
        );

        assert_eq!(
            parse_ip_from_url("https://10.0.0.50:3001/path"),
            Some("10.0.0.50".to_string())
        );

        assert_eq!(parse_ip_from_url("not-a-url"), None);
    }
}
