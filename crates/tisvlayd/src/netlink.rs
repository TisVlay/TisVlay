//! Kernel netlink utilities for network interface monitoring.
//!
//! Provides route resolution (TV IP → NIC name) and link carrier
//! monitoring via the kernel's rtnetlink interface. This layer works
//! without NetworkManager — it's kernel-level.
//!
//! Used by the daemon to:
//! - Resolve which NIC carries traffic to a TV at connection time
//! - Monitor carrier state for instant disconnect detection
//! - Detect route changes for interface failover

use futures_util::TryStreamExt;
use netlink_packet_route::link::LinkAttribute;
use netlink_packet_route::route::RouteAttribute;
use rtnetlink::{new_connection, RouteMessageBuilder};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use tracing::{debug, warn};

/// Resolve which network interface routes traffic to the given IP.
///
/// Queries the kernel routing table via rtnetlink (`RTM_GETROUTE`),
/// equivalent to `ip route get <ip>`. Returns the output interface
/// name (e.g., `"eno2"`) or `None` if no route exists.
///
/// Works correctly with bridges, VLANs, bonding, VPNs — the kernel
/// routing table is the source of truth.
pub async fn resolve_interface_for_ip(ip: IpAddr) -> Option<String> {
    let (connection, handle, _) = match new_connection() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Failed to create netlink connection for route resolution");
            return None;
        }
    };

    // Spawn the netlink connection handler
    let conn_task = tokio::spawn(connection);

    let result = resolve_route(&handle, ip).await;

    // Clean up: abort the connection task (it runs forever otherwise)
    conn_task.abort();

    result
}

async fn resolve_route(handle: &rtnetlink::Handle, ip: IpAddr) -> Option<String> {
    // Build a route query message for the destination IP
    let route_msg = match ip {
        IpAddr::V4(v4) => RouteMessageBuilder::<Ipv4Addr>::new()
            .destination_prefix(v4, 32)
            .build(),
        IpAddr::V6(v6) => RouteMessageBuilder::<Ipv6Addr>::new()
            .destination_prefix(v6, 128)
            .build(),
    };

    let mut routes = handle.route().get(route_msg).execute();

    while let Ok(Some(route)) = routes.try_next().await {
        // Extract the output interface index from route attributes
        let oif = route.attributes.iter().find_map(|attr| {
            if let RouteAttribute::Oif(index) = attr {
                Some(*index)
            } else {
                None
            }
        });

        if let Some(oif) = oif {
            // Look up the interface name by index
            let mut links = handle.link().get().match_index(oif).execute();
            if let Ok(Some(link)) = links.try_next().await {
                let name = link.attributes.iter().find_map(|attr| {
                    if let LinkAttribute::IfName(name) = attr {
                        Some(name.clone())
                    } else {
                        None
                    }
                });
                if let Some(name) = name {
                    debug!(ip = %ip, interface = %name, index = oif, "Resolved route");
                    return Some(name);
                }
            }
        }
    }

    debug!(ip = %ip, "No route found");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_resolve_localhost() {
        // Localhost should always route to "lo"
        let iface = resolve_interface_for_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)).await;
        assert_eq!(iface.as_deref(), Some("lo"));
    }
}
