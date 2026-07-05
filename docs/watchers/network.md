# Network Watcher Architecture

How the daemon detects network connectivity changes and scopes them to individual TVs using kernel netlink — no NetworkManager dependency.

## Kernel Netlink Monitoring

File: `crates/tisvlay-daemon/src/netlink.rs`

A single `rtnetlink` multicast connection subscribes to five groups:

| Group | Events | Daemon action |
|-------|--------|---------------|
| `RTMGRP_LINK` | Carrier up/down | `InEvent::Carrier { interface, up }` per NIC |
| `RTMGRP_IPV4_ROUTE` | Route add/delete | Re-resolve monitored TVs → `InEvent::RouteResolved` |
| `RTMGRP_IPV6_ROUTE` | Route add/delete | Re-resolve monitored TVs → `InEvent::RouteResolved` |
| `RTMGRP_IPV4_IFADDR` | IPv4 address add/remove | Re-resolve monitored TVs |
| `RTMGRP_IPV6_IFADDR` | IPv6 address add/remove | Re-resolve monitored TVs |

This is kernel-level, fires within milliseconds of a state change, and works without NetworkManager.

## Route-Based Device Resolution

For every network-transport TV, the daemon resolves which NIC carries traffic to its IP via `RTM_GETROUTE` (equivalent to `ip route get <IP>`):

```
ip route get 192.168.1.170
→ 192.168.1.170 dev eno2 src 192.168.1.79
```

The reactor requests monitoring per TV (`RouteRequest::Monitor`, carrying the connector's `primary_descriptor`); the netlink watcher answers with `InEvent::RouteResolved`, which the world stores as the `MonitoredInterface` component. This scopes carrier events to the correct NIC — a carrier drop on `wlan0` doesn't disconnect a TV routed through `eno2`.

The daemon never asks the user which interface to use — the kernel routing table is the source of truth. Works correctly with bridges, VLANs, bonding, VPNs, etc.

### Architectural layers

```
Vendor (LG, Epson, Sony)         ← backend crate
Protocol (SSAP, PJLink, HTTP)    ← backend crate
Transport (Network, Serial, ...)  ← TvConnector::transport()
NIC (eno2, wlan0, br0)           ← route lookup (daemon, rtnetlink)
Carrier state (up/down)          ← netlink RTMGRP_LINK (daemon)
```

Backends declare `Transport::Network` and provide the IP via `display_address()`. The daemon resolves IP → NIC → carrier. This keeps backends vendor-agnostic and transport-agnostic.

### When routes are resolved

- **Enrollment**: AddTv → monitor request → resolve → monitor that NIC
- **Every dial spawn**: a merged config (update-config, re-pair) re-registers, so an edited IP re-resolves
- **Route change events**: coalesced → re-resolve all monitored TVs

## Carrier Detection

A link is considered "usable" when **both** conditions are true:

- `IFF_LOWER_UP` (physical carrier — cable plugged in)
- `OperState::Up` (software state — link is operationally up)

This catches both:
- Cable pull: `IFF_LOWER_UP` goes false
- NM software disconnect (GUI/CLI): `OperState` goes to `Down` while carrier stays true (cable still plugged in)

## Route Transition Actions

When a `RouteResolved` changes a TV's monitored interface (`apply_route_resolutions` in `tisvlay-world`):

| Previous | Current | Action |
|----------|---------|--------|
| `Some("eno2")` | `None` | Park the connection (`CarrierParked`) |
| `Some("eno2")` \| parked | `Some(_)` | Redial (`PendingConnect`) |
| `Some("eno2")` | `Some("wlp7s0")` | Drop + redial if `!survives_source_ip_change()` |
| `Some("eno2")` | `Some("eno2")` | No change |

The NIC-change case checks `TvConnector::survives_source_ip_change()` — TCP/TLS protocols (WebSocket) return `false` (connection bound to 4-tuple), while future QUIC backends could return `true`. This is a connector-level property, not a transport category — see the design decision in `TvConnector` docs.

## Route Event Coalescing

NIC transitions fire dozens of route/address events over seconds. Route events are coalesced: the first event triggers immediate re-resolution, then a 100ms collection window drains queued duplicates before a final pass. This gives instant response with no per-event noise.

High-frequency route sampling (10ms, `scripts/bouncing_routes.py`) confirmed the kernel routing table never produces transient wrong results — `RTM_GETROUTE` is always consistent with the current FIB state. No debounce needed for correctness.

## Pitfalls Discovered

### RTM_F_LOOKUP_TABLE

Without `RouteFlags::LookupTable` on `RTM_GETROUTE` queries, the kernel returns cached/stale routes for deconfigured interfaces. Discovered via `strace` comparing our behavior with `ip route get`:

```rust
msg.header.flags.insert(RouteFlags::LookupTable);
```

### IPv6 link-local address persistence

After NM disconnect, `fe80::` link-local addresses persist on the interface. Without filtering, the address validation check would pass and the interface would appear "usable" despite having no routable address. Fix: only count IPv4 addresses or IPv6 addresses with `scope != Link` as "routable".

### Virtual bridges (virbr0, docker0)

NM global state = best state across all interfaces. With virbr0 "connected (externally)", NM stays at `ConnectedLocal` even after the real NIC loses carrier. Per-interface carrier monitoring via kernel netlink avoids this — each TV is scoped to its resolved NIC.

### NetworkManager watcher removed

An earlier implementation included an NM `StateChanged` watcher as a fallback. Testing showed NM's global state oscillates during NIC transitions, causing false disconnects that raced with the per-device carrier monitor. The kernel netlink layer handles all scenarios without NM, so the watcher was removed entirely.

## Failover Behavior

### Primary route only

When the primary NIC goes down:
1. Carrier lost → disconnect TV
2. Re-resolve route for TV IP
3. If a new route exists (failover NIC) → reconnect via new NIC
4. If no route exists → TV stays disconnected until a route appears

The kernel routing table is the source of truth — same approach as WireGuard and OpenVPN.

### Wired ↔ Wi-Fi switching

**With continuity** (both NICs up): route stays on the preferred NIC (lower metric). No action until one NIC goes down. High-frequency sampling confirmed zero transient route flaps during Wi-Fi bring-up.

**Without continuity** (gap between NICs): carrier lost → disconnect → new NIC comes up → route available → reconnect.

### Sleep/wake

Carrier/route parking only acts on live connections (`ConnectionHandle`); an in-flight dial (`Reconnecting`) fails and retries on its own schedule, so WoL retry loops during system resume are never fought.

## Crates

| Crate | Purpose |
|-------|---------|
| `rtnetlink` | Async route queries + link/route/address multicast |
| `netlink-packet-route` | Route/link message types |
| `netlink-packet-core` | Netlink message envelope |

## Tested Scenarios

| Scenario | Result | Mechanism |
|----------|--------|-----------|
| Cable pull | Instant disconnect | `RTMGRP_LINK` carrier detection |
| Cable pull with virbr0 | Instant disconnect | Per-interface, not NM global state |
| NM GUI disconnect | ~100ms detection | Route re-resolution + coalesce |
| NM GUI reconnect | ~100ms detection | Route reappears → redial |
| Wi-Fi connect while wired | No false trigger | Route stays on preferred NIC |
| Wired disconnect (failover) | Instant disconnect + reconnect via Wi-Fi | Carrier + route + `survives_source_ip_change` |
| Wired reconnect (failback) | NIC change → reconnect via wired | Route change + `survives_source_ip_change` |
| Sleep/wake | Unchanged | `reconnecting` flag prevents racing |
