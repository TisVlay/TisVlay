//! Kernel link-carrier monitor (ported from the root daemon's netlink
//! watcher): RTMGRP_LINK multicast → `InEvent::Carrier` transitions.
//! Fires within milliseconds of a cable pull, regardless of NetworkManager
//! state; route/address changes nudge parked connections with a carrier-up.

use std::collections::HashMap;

use futures_util::StreamExt;
use netlink_packet_core::NetlinkPayload;
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::link::{LinkAttribute, LinkFlags};
use rtnetlink::{MulticastGroup, new_multicast_connection};
use tisvlay_world::{InEvent, WorldClient};
use tracing::{info, trace, warn};

pub(crate) fn spawn_link_monitor(world: WorldClient) {
    tokio::spawn(async move {
        if let Err(e) = watch_link_carrier(world).await {
            warn!(error = %e, "link carrier monitor exited");
        }
    });
}

async fn watch_link_carrier(world: WorldClient) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, _handle, mut messages) = new_multicast_connection(&[
        MulticastGroup::Link,
        MulticastGroup::Ipv4Ifaddr,
        MulticastGroup::Ipv6Ifaddr,
        MulticastGroup::Ipv4Route,
        MulticastGroup::Ipv6Route,
    ])?;
    tokio::spawn(conn);
    info!("link carrier monitor started");

    let mut carrier_state: HashMap<u32, bool> = HashMap::new();
    while let Some((message, _)) = messages.next().await {
        let NetlinkPayload::InnerMessage(msg) = message.payload else {
            continue;
        };
        match msg {
            RouteNetlinkMessage::NewLink(link) => {
                let index = link.header.index;
                // Usable = physical carrier (cable) AND operational state
                // (software up) — same rule as the root watcher.
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
                if !world.send(InEvent::Carrier { up: usable }) {
                    break;
                }
            }
            RouteNetlinkMessage::NewRoute(_)
            | RouteNetlinkMessage::DelRoute(_)
            | RouteNetlinkMessage::NewAddress(_)
            | RouteNetlinkMessage::DelAddress(_) => {
                // Reachability may have been restored through a different
                // path — nudge parked connections to redial.
                trace!("route table changed, nudging parked connections");
                if !world.send(InEvent::Carrier { up: true }) {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
