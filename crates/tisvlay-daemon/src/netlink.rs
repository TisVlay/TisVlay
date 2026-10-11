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

// External-input profile: this module handles netlink route messages from
// the kernel, so indexing, arithmetic, string slicing and narrowing casts
// must be checked here (docs/build.md, Lint policy).
#![warn(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::cast_possible_truncation
)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt};
use netlink_packet_core::NetlinkPayload;
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::{LinkAttribute, LinkFlags, LinkMessage};
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

/// The monitor's view of the network: each link's usability and each
/// monitored TV's address and resolved interface. Pure bookkeeping; the
/// I/O loop feeds it kernel facts and forwards the events it returns.
#[derive(Debug, Default)]
struct MonitorState {
    carrier: HashMap<u32, bool>,
    monitored: HashMap<Uuid, IpAddr>,
    resolved: HashMap<Uuid, Option<String>>,
}

impl MonitorState {
    /// A link's current usability; the `Carrier` event when it changed.
    fn on_link(&mut self, index: u32, name: String, usable: bool) -> Option<InEvent> {
        (self.carrier.insert(index, usable) != Some(usable)).then_some(InEvent::Carrier {
            interface: name,
            up: usable,
        })
    }

    fn monitor(&mut self, tv: Uuid, ip: IpAddr) {
        self.monitored.insert(tv, ip);
    }

    fn forget(&mut self, tv: Uuid) {
        self.monitored.remove(&tv);
        self.resolved.remove(&tv);
    }

    /// A TV's freshly resolved interface; the `RouteResolved` event when
    /// it changed.
    fn on_resolved(&mut self, tv: Uuid, interface: Option<String>) -> Option<InEvent> {
        (self.resolved.get(&tv) != Some(&interface)).then(|| {
            self.resolved.insert(tv, interface.clone());
            InEvent::RouteResolved { tv, interface }
        })
    }

    /// The monitored TVs, for a re-resolution pass.
    fn monitored(&self) -> Vec<(Uuid, IpAddr)> {
        self.monitored.iter().map(|(&tv, &ip)| (tv, ip)).collect()
    }
}

/// A link message's index, name and usability: physical carrier (cable)
/// AND operational state (software up), the same rule as the root
/// watcher. `None` for a link without a name.
fn link_status(link: &LinkMessage) -> Option<(u32, String, bool)> {
    let physical = link.header.flags.contains(LinkFlags::LowerUp);
    let oper_up = link.attributes.iter().any(|attr| {
        matches!(
            attr,
            LinkAttribute::OperState(netlink_packet_route::link::State::Up)
        )
    });
    let name = link.attributes.iter().find_map(|attr| match attr {
        LinkAttribute::IfName(name) => Some(name.clone()),
        _ => None,
    })?;
    Some((link.header.index, name, physical && oper_up))
}

/// Forward an event to the world, logging it; false once the world is
/// gone.
fn send(world: &WorldClient, event: InEvent) -> bool {
    match &event {
        InEvent::Carrier { interface, up } => info!(%interface, up, "carrier transition"),
        InEvent::RouteResolved { tv, interface } => {
            info!(%tv, interface = ?interface, "route resolved");
        }
        _ => {}
    }
    world.send(event)
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

    let mut state = MonitorState::default();
    let mut settle_at: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            request = routes.recv() => {
                match request {
                    Some(RouteRequest::Monitor { tv, host }) => {
                        let Ok(ip) = host.parse::<IpAddr>() else {
                            debug!(%tv, host, "address is not an IP; skipping route monitoring");
                            state.forget(tv);
                            continue;
                        };
                        state.monitor(tv, ip);
                        let interface = resolve_interface_for_ip(ip).await;
                        if let Some(event) = state.on_resolved(tv, interface)
                            && !send(&world, event)
                        {
                            break;
                        }
                    }
                    Some(RouteRequest::Forget { tv }) => state.forget(tv),
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
                        let Some((index, name, usable)) = link_status(&link) else { continue };
                        if let Some(event) = state.on_link(index, name, usable)
                            && !send(&world, event)
                        {
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
                            if !re_resolve(&world, &mut state).await {
                                break;
                            }
                            settle_at = tokio::time::Instant::now().checked_add(ROUTE_SETTLE);
                        }
                    }
                    _ => {}
                }
            }
            () = async { tokio::time::sleep_until(settle_at.expect("the branch is enabled only while settle_at is Some")).await },
                if settle_at.is_some() =>
            {
                settle_at = None;
                if !re_resolve(&world, &mut state).await {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// One resolution pass over every monitored TV; sends `RouteResolved`
/// for changes only. Returns false when the world is gone.
async fn re_resolve(world: &WorldClient, state: &mut MonitorState) -> bool {
    for (tv, ip) in state.monitored() {
        let interface = resolve_interface_for_ip(ip).await;
        if let Some(event) = state.on_resolved(tv, interface)
            && !send(world, event)
        {
            return false;
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

    #[test]
    fn carrier_events_fire_on_changes_only() {
        let mut state = MonitorState::default();
        assert!(matches!(
            state.on_link(2, "eno1".into(), true),
            Some(InEvent::Carrier { up: true, .. })
        ));
        assert!(
            state.on_link(2, "eno1".into(), true).is_none(),
            "no change, no event"
        );
        assert!(matches!(
            state.on_link(2, "eno1".into(), false),
            Some(InEvent::Carrier { up: false, .. })
        ));
    }

    #[test]
    fn resolutions_fire_on_changes_only_and_forget_clears() {
        let mut state = MonitorState::default();
        let tv = Uuid::new_v4();
        state.monitor(tv, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(state.on_resolved(tv, Some("eno1".into())).is_some());
        assert!(state.on_resolved(tv, Some("eno1".into())).is_none());
        assert!(
            state.on_resolved(tv, None).is_some(),
            "a lost route is a change"
        );
        state.forget(tv);
        assert!(state.monitored().is_empty());
        assert!(
            state.on_resolved(tv, None).is_some(),
            "a forgotten TV's next resolution reports afresh"
        );
    }

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
        if !has_v6_loopback {
            eprintln!("no ::1 on this host: the v6 route arm is only checked for absence");
        }
        let iface = resolve_interface_for_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)).await;
        assert_eq!(iface.as_deref(), has_v6_loopback.then_some("lo"));
    }
}
