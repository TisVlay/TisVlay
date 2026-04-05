# Network Watcher Architecture

How the daemon detects network connectivity changes and scopes them to individual TVs. This covers both the kernel netlink layer and the NetworkManager fallback.

## Two-Layer Monitoring

### Layer 1: Kernel netlink (primary, always available)

File: `crates/tisvlayd/src/netlink.rs`

A single `rtnetlink` multicast connection subscribes to five groups:

| Group | Events | Daemon action |
|-------|--------|---------------|
| `RTMGRP_LINK` | Carrier up/down | `InterfaceUp` / `InterfaceDown` per NIC |
| `RTMGRP_IPV4_ROUTE` | Route add/delete | `RouteChanged` (re-resolve all TVs) |
| `RTMGRP_IPV6_ROUTE` | Route add/delete | `RouteChanged` (re-resolve all TVs) |
| `RTMGRP_IPV4_IFADDR` | IPv4 address add/remove | `RouteChanged` |
| `RTMGRP_IPV6_IFADDR` | IPv6 address add/remove | `RouteChanged` |

This layer works without NetworkManager — it's kernel-level, fires within milliseconds of a state change.

### Layer 2: NetworkManager global state (fallback)

File: `crates/tisvlayd/src/network_watcher.rs`

Monitors `org.freedesktop.NetworkManager` `StateChanged` on the system bus. Catches scenarios the netlink layer might miss (e.g., captive portal detection). Gracefully exits if NM is unavailable.

## Route-Based Device Resolution

When a network-transport TV connects, the daemon resolves which NIC carries traffic to its IP via `RTM_GETROUTE` (equivalent to `ip route get <IP>`):

```
ip route get 192.168.1.170
→ 192.168.1.170 dev eno2 src 192.168.1.79
```

The result is stored on `ManagedTv::monitored_interface`. This scopes carrier events to the correct NIC — a carrier drop on `wlan0` doesn't disconnect a TV routed through `eno2`.

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

- **Connection time**: TV connects → resolve route → monitor that NIC
- **Reconnection time**: new route may point to a different NIC
- **Route change events**: trailing-edge debounce → re-resolve all TVs

## Carrier Detection

A link is considered "usable" when **both** conditions are true:

- `IFF_LOWER_UP` (physical carrier — cable plugged in)
- `OperState::Up` (software state — NM considers the link operational)

This catches both:
- Cable pull: `IFF_LOWER_UP` goes false
- NM software disconnect (GUI/CLI): `OperState` goes to `Down` while carrier stays true (cable still plugged in)

### Route transition actions

When `resolve_monitored_interface` runs on a route change event:

| Previous | Current | Action |
|----------|---------|--------|
| `Some("eno2")` | `None` | Disconnect TV, emit `ConnectionChanged(false)` |
| `None` | `Some("eno2")` | Emit `InterfaceUp` to trigger reconnection |
| `Some("eno2")` | `Some("wlan0")` | Update monitored interface (failover) |
| `Some("eno2")` | `Some("eno2")` | No change |

The `Reconnect` case emits an event instead of calling `reconnect()` directly, avoiding recursive async.

## Pitfalls Discovered

### RTM_F_LOOKUP_TABLE

Without `RouteFlags::LookupTable` on `RTM_GETROUTE` queries, the kernel returns cached/stale routes for deconfigured interfaces. Discovered via `strace` comparing our behavior with `ip route get`:

```rust
msg.header.flags.insert(RouteFlags::LookupTable);
```

Without this flag, `resolve_interface_for_ip` would still return `Some("eno2")` after NM deconfigured the interface.

### IPv6 link-local address persistence

After NM disconnect, `fe80::` link-local addresses persist on the interface. Without filtering, the address validation check would pass and the interface would appear "usable" despite having no routable address.

Fix: only count IPv4 addresses or IPv6 addresses with `scope != Link` as "routable".

### Trailing-edge debounce for route changes

NM disconnect fires dozens of route/address events in a burst (one per route removed, one per address removed). A **leading-edge** debounce runs re-resolution on the first event — before routes are flushed — gets a stale result, then blocks subsequent events for the debounce window. The settled state is never checked.

A **trailing-edge** debounce resets a 500ms deadline on each event. Re-resolution runs 500ms after the last event, when the route table has fully settled. Implemented as a `tokio::select!` between the event channel and a `tokio::time::sleep_until(deadline)`.

### Virtual bridges (virbr0, docker0)

NM global state = best state across all interfaces. With virbr0 "connected (externally)", NM stays at `ConnectedLocal` even after the real NIC loses carrier. This is why per-interface carrier monitoring via kernel netlink is the primary layer — NM global state cannot distinguish which interface matters.

## Failover Behavior

### Primary route only

When the primary NIC goes down:
1. Carrier lost → disconnect TV
2. Re-resolve route for TV IP
3. If a new route exists (failover NIC) → reconnect via new NIC
4. If no route exists → TV stays disconnected until a route appears

The kernel routing table is the source of truth — same approach as WireGuard and OpenVPN.

### Wired ↔ Wi-Fi switching

**With continuity** (both NICs up briefly): TCP connection survives if the IP doesn't change. Route shifts from `eno2` to `wlan0`; daemon updates monitored NIC on the next route event.

**Without continuity** (gap between NICs): carrier lost → disconnect → Wi-Fi comes up → route available → reconnect.

### NM/WoL racing during wake

NM oscillates during system wake (`restored → lost → restored` within ~100ms). A per-TV `reconnecting` flag guards both `reconnect_disconnected` and `disconnect_network_tvs`, preventing NM events from racing with in-flight WoL retry loops.

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
| NM GUI disconnect | ~500ms detection | Trailing-edge debounce on route/addr events |
| NM GUI reconnect | ~100ms detection | Route reappears → `InterfaceUp` |
| nmcli disconnect/connect | Same as NM GUI | Same mechanism |
| Sleep/wake cycle | Unchanged | `reconnecting` flag prevents racing |
