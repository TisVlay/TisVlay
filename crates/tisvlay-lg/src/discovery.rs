//! LG WebOS TV discovery via SSDP.
//!
//! Discovers LG WebOS TVs on the local network using SSDP multicast.

use futures_util::StreamExt;
use ssdp_client::{SearchTarget, URN};
use std::collections::HashMap;
use std::time::Duration;
use tisvlay_core::{DiscoveredTv, Result};

/// Discover LG TVs on the network using SSDP.
///
/// Sends an M-SEARCH request to the SSDP multicast address and collects
/// responses from LG WebOS TVs.
///
/// # Arguments
///
/// * `timeout` - How long to wait for TV responses
///
/// # Returns
///
/// A list of discovered TVs. Returns an empty list if no TVs are found
/// (not finding TVs is not an error).
pub async fn discover_lg_tvs(timeout: Duration) -> Result<Vec<DiscoveredTv>> {
    tracing::debug!(
        timeout_ms = timeout.as_millis(),
        "Starting LG SSDP discovery"
    );

    // Deduplicate by IP address since each TV exposes multiple UPnP services
    // with different UUIDs. We prefer entries with DLNA device names.
    let mut tvs: HashMap<String, DiscoveredTv> = HashMap::new();

    // Try multiple search targets for better compatibility.
    // Different LG TV models/firmware respond to different targets:
    // - Some respond only to the LG-specific URN
    // - Most respond to the generic MediaRenderer URN
    // - RootDevice catches any remaining UPnP devices
    let targets: &[SearchTarget] = &[
        SearchTarget::URN(URN::service("lge-com", "webostv", 1)),
        SearchTarget::URN(URN::device("schemas-upnp-org", "MediaRenderer", 1)),
        SearchTarget::RootDevice,
    ];

    // Divide timeout among targets (searched sequentially)
    let per_target_timeout = timeout / targets.len() as u32;

    for target in targets {
        tracing::debug!(?target, "Searching with target");

        match ssdp_client::search(target, per_target_timeout, 2, None).await {
            Ok(mut responses) => {
                while let Some(response) = responses.next().await {
                    if let Ok(resp) = response {
                        if is_lg_tv(&resp) {
                            if let Some(tv) = parse_response(&resp) {
                                tracing::debug!(id = %tv.id, name = %tv.name, address = %tv.address, "Discovered LG TV");
                                // Deduplicate by address, preferring entries with real DLNA names
                                // over fallback names like "LG TV (ip)"
                                let dominated = tvs.get(&tv.address).is_some_and(|existing| {
                                    has_dlna_name(existing) && !has_dlna_name(&tv)
                                });
                                if !dominated {
                                    tvs.insert(tv.address.clone(), tv);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, ?target, "SSDP search failed");
            }
        }
    }

    tracing::info!(count = tvs.len(), "LG SSDP discovery complete");
    Ok(tvs.into_values().collect())
}

/// Check if a discovered TV has a real DLNA device name (vs fallback).
fn has_dlna_name(tv: &DiscoveredTv) -> bool {
    tv.name.starts_with("[LG]")
}

/// Check if an SSDP response is from an LG WebOS TV.
fn is_lg_tv(resp: &ssdp_client::SearchResponse) -> bool {
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
fn parse_response(resp: &ssdp_client::SearchResponse) -> Option<DiscoveredTv> {
    let id = extract_uuid_from_usn(resp.usn())?;
    let location = resp.location();
    let ip = parse_ip_from_url(location)?;

    let name = resp
        .extra_header("DLNADeviceName.lge.com")
        .and_then(|s| urlencoding::decode(s).ok())
        .map(|s| s.into_owned())
        .unwrap_or_else(|| format!("LG TV ({})", ip));

    Some(DiscoveredTv {
        id,
        name,
        address: ip,
        hardware_id: String::new(),
        vendor: crate::VENDOR_ID.to_string(),
        location: location.to_string(),
    })
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
        .map(|s| s.to_string())
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
