//! Neighbour-table MAC lookup for discovery enrichment (ported from the
//! root daemon's netlink helper): SSDP replies and pairing traffic have
//! just primed the kernel's neighbour table, so a discovered TV's MAC —
//! the Wake-on-LAN prerequisite in the LG vendor config — is one
//! `RTM_GETNEIGH` dump away. Netlink rather than `/proc/net/arp`: the
//! packaged unit runs `ProcSubset=pid`, which mounts a `/proc` without
//! `/proc/net`, while `AF_NETLINK` is already open for the carrier
//! monitor.

use std::net::IpAddr;

use futures_util::TryStreamExt;
use netlink_packet_route::neighbour::{NeighbourAddress, NeighbourAttribute, NeighbourState};
use rtnetlink::{IpVersion, new_connection};
use tracing::warn;

/// Look up the MAC address for `ip` from the kernel neighbour table
/// (equivalent to `ip neighbour show`). `None` when the address doesn't
/// parse as an IP, isn't in the table (cross-VLAN, wrong subnet), or the
/// entry has no resolved link-layer address yet — callers treat that as
/// "ask the user for a MAC", not a hard failure.
pub(crate) async fn lookup_mac(ip: &str) -> Option<String> {
    let ip: IpAddr = ip.parse().ok()?;
    let (connection, handle, _) = match new_connection() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to create netlink connection for neighbour lookup");
            return None;
        }
    };
    let conn_task = tokio::spawn(connection);
    let result = neighbour_lookup(&handle, ip).await;
    conn_task.abort();
    result
}

async fn neighbour_lookup(handle: &rtnetlink::Handle, ip: IpAddr) -> Option<String> {
    let ip_version = match ip {
        IpAddr::V4(_) => IpVersion::V4,
        IpAddr::V6(_) => IpVersion::V6,
    };
    let mut neighbours = handle.neighbours().get().set_family(ip_version).execute();
    while let Ok(Some(msg)) = neighbours.try_next().await {
        // Skip entries with no resolved link-layer address.
        match msg.header.state {
            NeighbourState::Reachable
            | NeighbourState::Stale
            | NeighbourState::Permanent
            | NeighbourState::Probe
            | NeighbourState::Delay
            | NeighbourState::Noarp => {}
            _ => continue,
        }
        let mut dest: Option<IpAddr> = None;
        let mut lladdr: Option<[u8; 6]> = None;
        for attr in &msg.attributes {
            match attr {
                NeighbourAttribute::Destination(NeighbourAddress::Inet(v4)) => {
                    dest = Some(IpAddr::V4(*v4));
                }
                NeighbourAttribute::Destination(NeighbourAddress::Inet6(v6)) => {
                    dest = Some(IpAddr::V6(*v6));
                }
                NeighbourAttribute::LinkLayerAddress(bytes) if bytes.len() == 6 => {
                    let mut mac = [0u8; 6];
                    mac.copy_from_slice(bytes);
                    lladdr = Some(mac);
                }
                _ => {}
            }
        }
        if dest != Some(ip) {
            continue;
        }
        let mac = lladdr?;
        return Some(format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn non_ip_addresses_short_circuit() {
        assert_eq!(lookup_mac("mock://den").await, None);
    }

    #[tokio::test]
    async fn absent_entries_resolve_to_none() {
        // TEST-NET-3 (RFC 5737) is never in a real neighbour table.
        assert_eq!(lookup_mac("203.0.113.254").await, None);
    }
}
