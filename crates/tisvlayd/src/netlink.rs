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
use tracing::{debug, info, trace, warn};

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
    // Build a route query message for the destination IP.
    // Set LookupTable flag to match `ip route get` behavior — without it,
    // the kernel may return cached/stale routes for deconfigured interfaces.
    use netlink_packet_route::route::RouteFlags;
    let route_msg = match ip {
        IpAddr::V4(v4) => {
            let mut msg = RouteMessageBuilder::<Ipv4Addr>::new()
                .destination_prefix(v4, 32)
                .build();
            msg.header.flags.insert(RouteFlags::LookupTable);
            msg
        }
        IpAddr::V6(v6) => {
            let mut msg = RouteMessageBuilder::<Ipv6Addr>::new()
                .destination_prefix(v6, 128)
                .build();
            msg.header.flags.insert(RouteFlags::LookupTable);
            msg
        }
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
                    // Verify the interface has a non-link-local IP address.
                    // NM software disconnect removes routable IPs but
                    // IPv6 link-local (fe80::) persists. Check that the
                    // interface has a usable address for TV communication.
                    use netlink_packet_route::AddressFamily;
                    let mut addrs = handle.address().get().set_link_index_filter(oif).execute();
                    let mut has_routable_addr = false;
                    while let Ok(Some(addr_msg)) = addrs.try_next().await {
                        // Skip IPv6 link-local (family=AF_INET6, scope=253/link)
                        if addr_msg.header.family == AddressFamily::Inet {
                            has_routable_addr = true;
                            break;
                        }
                        if addr_msg.header.family == AddressFamily::Inet6
                            && !matches!(
                                addr_msg.header.scope,
                                netlink_packet_route::address::AddressScope::Link
                            )
                        {
                            has_routable_addr = true;
                            break;
                        }
                    }
                    let has_addr = has_routable_addr;
                    if !has_addr {
                        debug!(ip = %ip, interface = %name, "Route exists but interface has no IP address");
                        return None;
                    }
                    trace!(ip = %ip, interface = %name, index = oif, "Resolved route");
                    return Some(name);
                }
            }
        }
    }

    debug!(ip = %ip, "No route found");
    None
}

/// Spawn a background task that monitors kernel link state changes.
///
/// Subscribes to `RTMGRP_LINK` via netlink multicast. When an interface's
/// `IFF_LOWER_UP` flag changes (carrier up/down), sends an `InterfaceUp`
/// or `InterfaceDown` event through the state change channel.
///
/// This is the primary disconnect detection mechanism — it fires within
/// milliseconds of a cable pull, regardless of NM state or virtual bridges.
pub fn spawn_link_monitor(
    tx: tokio::sync::mpsc::UnboundedSender<crate::tv_manager::StateChangeEvent>,
) {
    tokio::spawn(async move {
        if let Err(e) = watch_link_carrier(tx).await {
            warn!(error = %e, "Link carrier monitor exited");
        }
    });
}

async fn watch_link_carrier(
    tx: tokio::sync::mpsc::UnboundedSender<crate::tv_manager::StateChangeEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::StreamExt;
    use netlink_packet_core::NetlinkPayload;
    use netlink_packet_route::link::LinkFlags;
    use netlink_packet_route::RouteNetlinkMessage;
    use rtnetlink::{new_multicast_connection, MulticastGroup};
    use std::collections::HashMap;

    let (conn, _handle, mut messages) = new_multicast_connection(&[
        MulticastGroup::Link,
        MulticastGroup::Ipv4Ifaddr,
        MulticastGroup::Ipv6Ifaddr,
        MulticastGroup::Ipv4Route,
        MulticastGroup::Ipv6Route,
    ])?;
    tokio::spawn(conn);

    info!("Link carrier monitor started");

    // Track carrier state per interface index to detect transitions.
    let mut carrier_state: HashMap<u32, bool> = HashMap::new();

    while let Some((message, _)) = messages.next().await {
        if let NetlinkPayload::InnerMessage(msg) = message.payload {
            match msg {
                RouteNetlinkMessage::NewLink(link_msg) => {
                    let index = link_msg.header.index;

                    // Check both physical carrier (IFF_LOWER_UP) and operational
                    // state (IFLA_OPERSTATE). Physical carrier tracks cable
                    // plug/unplug; OperState tracks software disconnect (NM GUI,
                    // nmcli). A link is usable when both are true.
                    let physical_carrier = link_msg.header.flags.contains(LinkFlags::LowerUp);
                    let oper_up = link_msg.attributes.iter().any(|attr| {
                        matches!(
                            attr,
                            LinkAttribute::OperState(netlink_packet_route::link::State::Up)
                        )
                    });
                    let is_usable = physical_carrier && oper_up;

                    // Find the interface name from attributes
                    let name = link_msg.attributes.iter().find_map(|attr| {
                        if let LinkAttribute::IfName(name) = attr {
                            Some(name.clone())
                        } else {
                            None
                        }
                    });

                    let Some(name) = name else { continue };

                    // Check for state transition
                    let prev = carrier_state.insert(index, is_usable);
                    if prev == Some(is_usable) {
                        continue; // No change
                    }

                    if is_usable {
                        info!(interface = %name, "Carrier up");
                        if tx
                            .send(crate::tv_manager::StateChangeEvent::InterfaceUp {
                                interface: name,
                            })
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        info!(interface = %name, "Carrier down");
                        if tx
                            .send(crate::tv_manager::StateChangeEvent::InterfaceDown {
                                interface: name,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                RouteNetlinkMessage::NewRoute(_)
                | RouteNetlinkMessage::DelRoute(_)
                | RouteNetlinkMessage::NewAddress(_)
                | RouteNetlinkMessage::DelAddress(_) => {
                    trace!("Route table changed, triggering interface re-resolution");
                    if tx
                        .send(crate::tv_manager::StateChangeEvent::RouteChanged)
                        .is_err()
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
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
