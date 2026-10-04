//! Kernel network monitor (ported from the root daemon's netlink
//! watcher): route resolution (TV IP → NIC name) plus link-carrier
//! monitoring via rtnetlink. Kernel-level — fires within milliseconds of
//! a cable pull and needs no NetworkManager.
//!
//! The reactor requests monitoring per network TV ([`RouteRequest`]);
//! this module answers with `InEvent::RouteResolved` (at request time
//! and whenever the routing table changes) and streams scoped
//! `InEvent::Carrier` transitions per NIC. The world matches both
//! against each TV's monitored interface — a wifi drop never parks a TV
//! riding ethernet, and an NM software disconnect (which keeps carrier
//! up but withdraws addresses/routes) surfaces as a lost route.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt};
use netlink_packet_core::NetlinkPayload;
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::{LinkAttribute, LinkFlags};
use netlink_packet_route::route::RouteAttribute;
use rtnetlink::{MulticastGroup, RouteMessageBuilder, new_connection, new_multicast_connection};
use tisvlay_world::{InEvent, RouteRequest, WorldClient};
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

/// Trailing re-resolution pass after a burst of route/address events: the
/// first event resolves immediately, then one more pass after the burst
/// settles. NIC transitions fire dozens of events over seconds.
const ROUTE_SETTLE: Duration = Duration::from_millis(100);

pub(crate) fn spawn_link_monitor(
    world: WorldClient,
    routes: mpsc::UnboundedReceiver<RouteRequest>,
) {
    tokio::spawn(async move {
        if let Err(e) = watch_network(world, routes).await {
            warn!(error = %e, "network monitor exited");
        }
    });
}

async fn watch_network(
    world: WorldClient,
    mut routes: mpsc::UnboundedReceiver<RouteRequest>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, _handle, mut messages) = new_multicast_connection(&[
        MulticastGroup::Link,
        MulticastGroup::Ipv4Ifaddr,
        MulticastGroup::Ipv6Ifaddr,
        MulticastGroup::Ipv4Route,
        MulticastGroup::Ipv6Route,
    ])?;
    tokio::spawn(conn);
    info!("network monitor started");

    let mut carrier_state: HashMap<u32, bool> = HashMap::new();
    let mut monitored: HashMap<Uuid, IpAddr> = HashMap::new();
    let mut resolved: HashMap<Uuid, Option<String>> = HashMap::new();
    let mut settle_at: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            request = routes.recv() => {
                match request {
                    Some(RouteRequest::Monitor { tv, host }) => {
                        let Ok(ip) = host.parse::<IpAddr>() else {
                            debug!(%tv, host, "address is not an IP; skipping route monitoring");
                            monitored.remove(&tv);
                            resolved.remove(&tv);
                            continue;
                        };
                        monitored.insert(tv, ip);
                        let interface = resolve_interface_for_ip(ip).await;
                        if resolved.get(&tv) != Some(&interface) {
                            info!(%tv, interface = ?interface, "route resolved");
                            resolved.insert(tv, interface.clone());
                            if !world.send(InEvent::RouteResolved { tv, interface }) {
                                break;
                            }
                        }
                    }
                    Some(RouteRequest::Forget { tv }) => {
                        monitored.remove(&tv);
                        resolved.remove(&tv);
                    }
                    None => break,
                }
            }
            message = messages.next() => {
                let Some((message, _)) = message else { break };
                let NetlinkPayload::InnerMessage(msg) = message.payload else {
                    continue;
                };
                match msg {
                    RouteNetlinkMessage::NewLink(link) => {
                        let index = link.header.index;
                        // Usable = physical carrier (cable) AND operational
                        // state (software up) — same rule as the root watcher.
                        let physical = link.header.flags.contains(LinkFlags::LowerUp);
                        let oper_up = link.attributes.iter().any(|attr| {
                            matches!(
                                attr,
                                LinkAttribute::OperState(netlink_packet_route::link::State::Up)
                            )
                        });
                        let usable = physical && oper_up;
                        let name = link.attributes.iter().find_map(|attr| match attr {
                            LinkAttribute::IfName(name) => Some(name.clone()),
                            _ => None,
                        });
                        let Some(name) = name else { continue };
                        if carrier_state.insert(index, usable) == Some(usable) {
                            continue;
                        }
                        info!(interface = %name, up = usable, "carrier transition");
                        if !world.send(InEvent::Carrier { interface: name, up: usable }) {
                            break;
                        }
                    }
                    RouteNetlinkMessage::NewRoute(_)
                    | RouteNetlinkMessage::DelRoute(_)
                    | RouteNetlinkMessage::NewAddress(_)
                    | RouteNetlinkMessage::DelAddress(_) => {
                        // Reachability may have changed in either direction —
                        // re-resolve every monitored TV. Immediate pass on the
                        // first event of a burst, one trailing pass after it
                        // settles.
                        trace!("route table changed, re-resolving monitored TVs");
                        if settle_at.is_none() {
                            if !re_resolve(&world, &monitored, &mut resolved).await {
                                break;
                            }
                            settle_at = Some(tokio::time::Instant::now() + ROUTE_SETTLE);
                        }
                    }
                    _ => {}
                }
            }
            _ = async { tokio::time::sleep_until(settle_at.unwrap()).await },
                if settle_at.is_some() =>
            {
                settle_at = None;
                if !re_resolve(&world, &monitored, &mut resolved).await {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// One resolution pass over every monitored TV; sends `RouteResolved`
/// for changes only. Returns false when the world is gone.
async fn re_resolve(
    world: &WorldClient,
    monitored: &HashMap<Uuid, IpAddr>,
    resolved: &mut HashMap<Uuid, Option<String>>,
) -> bool {
    for (&tv, &ip) in monitored {
        let interface = resolve_interface_for_ip(ip).await;
        if resolved.get(&tv) != Some(&interface) {
            info!(%tv, interface = ?interface, "route resolution changed");
            resolved.insert(tv, interface.clone());
            if !world.send(InEvent::RouteResolved { tv, interface }) {
                return false;
            }
        }
    }
    true
}

/// Resolve which network interface routes traffic to the given IP.
///
/// Queries the kernel routing table via rtnetlink (`RTM_GETROUTE`),
/// equivalent to `ip route get <ip>`. Returns the output interface
/// name (e.g., `"eno2"`) or `None` if no usable route exists.
///
/// Works correctly with bridges, VLANs, bonding, VPNs — the kernel
/// routing table is the source of truth.
async fn resolve_interface_for_ip(ip: IpAddr) -> Option<String> {
    let (connection, handle, _) = match new_connection() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "failed to create netlink connection for route resolution");
            return None;
        }
    };
    let conn_task = tokio::spawn(connection);
    let result = resolve_route(&handle, ip).await;
    conn_task.abort();
    result
}

async fn resolve_route(handle: &rtnetlink::Handle, ip: IpAddr) -> Option<String> {
    // LookupTable matches `ip route get` behavior — without it the
    // kernel may return cached/stale routes for deconfigured interfaces.
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

    let mut route_replies = handle.route().get(route_msg).execute();
    while let Ok(Some(route)) = route_replies.try_next().await {
        let oif = route.attributes.iter().find_map(|attr| match attr {
            RouteAttribute::Oif(index) => Some(*index),
            _ => None,
        });
        let Some(oif) = oif else { continue };

        let mut links = handle.link().get().match_index(oif).execute();
        let Ok(Some(link)) = links.try_next().await else {
            continue;
        };
        let name = link.attributes.iter().find_map(|attr| match attr {
            LinkAttribute::IfName(name) => Some(name.clone()),
            _ => None,
        });
        let Some(name) = name else { continue };

        // Verify the interface holds a routable address: an NM software
        // disconnect can leave a stale route while only IPv6 link-local
        // (fe80::) remains on the NIC.
        use netlink_packet_route::AddressFamily;
        let mut addrs = handle.address().get().set_link_index_filter(oif).execute();
        let mut has_routable_addr = false;
        while let Ok(Some(addr_msg)) = addrs.try_next().await {
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
        if !has_routable_addr {
            debug!(ip = %ip, interface = %name, "route exists but interface has no usable address");
            return None;
        }
        trace!(ip = %ip, interface = %name, index = oif, "resolved route");
        return Some(name);
    }

    debug!(ip = %ip, "no route found");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_localhost_to_lo() {
        let iface = resolve_interface_for_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)).await;
        assert_eq!(iface.as_deref(), Some("lo"));
    }

    #[tokio::test]
    async fn resolves_localhost_v6_to_lo() {
        // Exercises the v6 arm of resolve_route against the real kernel so
        // a refactor can't quietly break it while daemon traffic stays v4.
        // A host with IPv6 disabled (a container's default) has no ::1 and
        // so no route to it; the expectation follows the host rather than
        // assuming either, so the assertion is exact on both.
        let has_v6_loopback = std::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).is_ok();
        let iface = resolve_interface_for_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)).await;
        assert_eq!(iface.as_deref(), has_v6_loopback.then_some("lo"));
    }
}
