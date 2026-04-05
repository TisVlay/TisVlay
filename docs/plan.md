# Plan: Per-device carrier monitoring for accurate disconnect detection

## Problem

The NM watcher monitors **global** NetworkManager state (`StateChanged`).
When virtual bridges (virbr0, docker0) exist, NM's global state remains
`ConnectedLocal` even after the real NIC loses carrier. The daemon shows
the TV as connected for 20+ seconds after cable pull because NM doesn't
reach `Disconnected` until its connectivity check times out.

## Root cause

NM global state = best state across all interfaces. With virbr0 "connected
(externally)", the global state stays above our `has_network` threshold
even though the NIC carrying TV traffic is down.

## Design

### Two-layer monitoring

**Layer 1: Kernel netlink (primary, always available)**

The `rtnetlink` crate provides async event-driven monitoring of:
- **Route changes** (`RTMGRP_ROUTE`) — detect when the route to a TV's
  IP changes (interface failover, DHCP renewal)
- **Link state** (`RTMGRP_LINK`) — detect carrier up/down instantly
  via `IFF_LOWER_UP` flag

This layer works without NetworkManager — it's kernel-level.

**Layer 2: NM global state (fallback, when NM is available)**

Keep the existing `StateChanged` watcher as a fallback. It catches
scenarios the netlink layer might miss (e.g., NM connectivity checks
detecting a captive portal).

### Route-based device resolution

When a network-transport TV connects, the daemon resolves which NIC
carries traffic to its IP via the kernel routing table (rtnetlink
`RTM_GETROUTE`). This is the equivalent of `ip route get <IP>`:

```
ip route get 192.168.1.170
→ 192.168.1.170 dev eno2 src 192.168.1.79
```

This maps: TV IP → NIC name (e.g., `eno2`).

The daemon doesn't need to ask the user which interface to use —
the kernel routing table already knows. This works correctly with
bridges, VLANs, bonding, VPNs, etc.

### Architectural layers

```
Vendor (LG, Epson, Sony)         ← backend crate
Protocol (SSAP, PJLink, HTTP)    ← backend crate
Transport (Network, Serial, ...)  ← TvConnector::transport()
NIC (eno2, wlan0, br0)           ← route lookup (daemon, rtnetlink)
Carrier state (up/down)          ← netlink RTMGRP_LINK (daemon)
```

Backends declare `Transport::Network` and provide the IP via
`display_address()`. The daemon resolves IP → NIC → carrier. This
keeps backends vendor-agnostic and protocol-agnostic.

### Relevant Rust crates

| Crate | Purpose | Status |
|-------|---------|--------|
| `rtnetlink` v0.20 | Async route queries + link/route multicast monitoring | Active, production-grade |
| `netlink-packet-route` v0.28 | Route/link message types (`RouteAttribute`, `LinkFlags`, etc.) | Active, required by rtnetlink |
| `netlink-packet-core` v0.8 | Netlink message envelope (`NetlinkPayload`) | Active, required by rtnetlink |

`rtnetlink` is the primary tool — kernel-level, no NM dependency,
event-driven, async. `nmrs` was evaluated but not needed — raw
rtnetlink provides everything we need without NM API coupling.

### NM remains optional

All NM-specific monitoring (global state, device properties) is
gated on NM availability — if NM isn't running, the watcher logs
a warning and exits. The kernel netlink layer works regardless.

---

## Answers to open questions

### Q1: When to resolve the route?

Resolve at **connection time** and at **reconnection time**. If the
route changes between sessions (DHCP, failover), the reconnect path
picks up the new route. The mapping is: TV connects → resolve route →
monitor that NIC's carrier. If the NIC goes down and comes back, the
reconnect re-resolves (may find a different NIC if routes changed).

### Q2: Multiple routes / failover

**Primary route only.** When the primary NIC goes down:
1. Carrier lost → disconnect TV
2. Re-resolve route for TV IP
3. If a new route exists (failover NIC) → reconnect via new NIC,
   monitor new NIC's carrier
4. If no route exists → TV stays disconnected until a route appears

No need to monitor all possible paths simultaneously. The kernel
routing table is the source of truth — if a failover NIC takes over,
`ip route get` returns the new path. We follow the kernel's decision.

If the **original NIC comes back**, two scenarios:
- If the TV is already connected via the failover NIC → do nothing
  (connection is working). The kernel may or may not switch the route
  back depending on metrics. If it does, the next reconnect cycle
  picks up the change.
- If the TV is disconnected → re-resolve finds the original NIC →
  reconnect.

This is the same approach WireGuard and OpenVPN use: follow the OS
routing table, don't second-guess it.

### Q3: Wired ↔ Wi-Fi switching

**With continuity (both NICs up briefly):** The TV's TCP connection
survives if the IP doesn't change (same subnet). The route may shift
from `eno2` to `wlan0` — the daemon detects this on the next
RTMGRP_ROUTE event and updates the monitored NIC. No disconnect.

**Without continuity (gap between NICs):** The wired NIC goes down
→ carrier lost → TV disconnected. Wi-Fi NIC comes up → route to TV
IP becomes available → reconnect via Wi-Fi. Same as failover (Q2).

For **carrier detection on Wi-Fi**: kernel netlink `IFF_LOWER_UP`
works for both wired and wireless interfaces. NM's `Device.Wired.Carrier`
is wired-only, but the kernel flag is universal. Using rtnetlink
RTMGRP_LINK avoids this NM limitation.

### Q4: Where to track NM device / NIC on ManagedTv?

Looking at our prior design research (OpenHAB Bridge pattern, Home
Assistant connectivity model), the pattern is:

- **Connection state is a property of the connection instance**
- **Transport details are tracked by the daemon, not the backend**

`ManagedTv` already tracks `connection`, `powered_off`, `reconnecting`,
`last_known_input`. Adding `monitored_interface: Option<String>` (the
NIC name, e.g., `"eno2"`) fits naturally — it's connection lifecycle
state that the daemon owns.

This is **not** a backend concern (the LG backend doesn't know or care
about NICs). It's **not** a config concern (the user doesn't specify
it). It's runtime state resolved from the routing table, stored
alongside other runtime state on `ManagedTv`.

```rust
struct ManagedTv {
    config: TvConfig,
    connection: Option<Box<dyn TvConnection>>,
    powered_off: bool,
    reconnecting: bool,
    system_profile: ResolvedSystemProfile,
    last_known_input: Option<String>,
    /// NIC carrying traffic to this TV, resolved from routing table.
    /// Used to scope carrier monitoring to the correct interface.
    monitored_interface: Option<String>,
}
```

---

## Implementation plan

### Phase 1: Add `rtnetlink` dependency and route resolution

Add `rtnetlink` to tisvlayd. Implement async route resolution:

```rust
/// Resolve which network interface routes to the given IP.
/// Returns the interface name (e.g., "eno2") or None if no route.
async fn resolve_interface_for_ip(ip: IpAddr) -> Option<String>;
```

Call at connection time, store result on `ManagedTv::monitored_interface`.

### Phase 2: Kernel link monitoring via RTMGRP_LINK

Subscribe to netlink `RTMGRP_LINK` events. When `IFF_LOWER_UP` goes
false on a monitored interface, disconnect only the TVs using that
interface.

### Phase 3: Route change monitoring via RTMGRP_ROUTE

Subscribe to netlink `RTMGRP_IPV4_ROUTE` + `RTMGRP_IPV6_ROUTE` events
(combined with `RTMGRP_LINK` in a single `new_multicast_connection`).
On `NewRoute`/`DelRoute`, call `re_resolve_all_interfaces()` which
re-resolves every connected TV's `monitored_interface` from the
routing table. Route events don't identify which TV was affected,
so re-resolving all is the correct approach.

### Phase 4: Keep NM global state as fallback

Existing NM watcher remains. Per-interface carrier is the primary
signal; NM global state is the safety net.

### Phase 5: Test matrix

- [x] Cable pull on primary NIC: instant disconnect (carrier down via RTMGRP_LINK)
- [x] Cable pull with virbr0 present: instant disconnect (per-interface, not global NM state)
- [x] NM GUI disconnect: ~500ms detection (trailing-edge debounce on route/addr events)
- [x] NM GUI reconnect: ~100ms detection
- [x] nmcli device disconnect/connect: same as NM GUI
- [x] Sleep/wake cycle: unchanged (reconnecting flag prevents NM/WoL racing)
- [ ] Wi-Fi → wired failover: seamless reconnect (untested)
- [ ] No NM installed: kernel netlink still works (untested)

### Phase 6: Daemon lifecycle (post-carrier)

Graceful shutdown and warm restart were needed to complement carrier
monitoring — without them, a daemon restart left stale SSAP sessions
on the TV, causing "Connection reset" on reconnect.

- [x] Graceful shutdown: close all TV WebSocket sessions before D-Bus
  name release (c4be594)
- [x] Warm restart: proactively connect to all configured TVs on process
  restart (not system boot), using boot marker in /run/tisvlay/ (c4be594)
- [x] Move connection retry from daemon to LG backend — the 5-attempt
  retry for stale sessions is an LG/SSAP concern (d6ff212)

### Fixes discovered during carrier testing

- [x] RTM_F_LOOKUP_TABLE: kernel returned cached routes for deconfigured
  interfaces; fixed by setting RouteFlags::LookupTable on RTM_GETROUTE
  queries (36471c4)
- [x] IPv6 link-local persistence: fe80:: addresses survive NM disconnect;
  fixed by filtering to only count IPv4 or non-link-local IPv6 as
  "routable" (36471c4)
- [x] Leading-edge debounce missed settled state: replaced with 500ms
  trailing-edge debounce that fires after the last event in the
  burst (36471c4)
- [x] OperState check: link "usable" now requires both IFF_LOWER_UP
  (physical carrier) AND OperState::Up (software state) (36471c4)
- [x] Subscribe to RTMGRP_IPV4_IFADDR/RTMGRP_IPV6_IFADDR for IP
  address add/remove events (36471c4)

---

## Checklist

- [x] Phase 1: rtnetlink route resolution (resolve_interface_for_ip, monitored_interface on ManagedTv)
- [x] Phase 2: Kernel link monitoring (RTMGRP_LINK, InterfaceUp/InterfaceDown events)
- [x] Phase 3: Route change monitoring (RTMGRP_ROUTE, RouteChanged re-resolves interfaces)
- [x] Phase 4: Keep NM global state as fallback (existing watcher + per-interface primary)
- [x] Phase 5: Test matrix (cable pull, NM disconnect/reconnect, sleep/wake verified)
- [x] Phase 6: Daemon lifecycle (graceful shutdown, warm restart, retry refactor)
- [ ] Update design.md and CLAUDE.md

---

## References

- [rtnetlink crate](https://crates.io/crates/rtnetlink) (v0.18, async)
- [nmrs crate](https://crates.io/crates/nmrs) (v2.0, zbus-based NM bindings)
- [netlink(7)](https://man7.org/linux/man-pages/man7/netlink.7.html)
- [rtnetlink(7)](https://man7.org/linux/man-pages/man7/rtnetlink.7.html)
- [Monitoring Linux Networking State Using Netlink](https://olegkutkov.me/2018/02/14/monitoring-linux-networking-state-using-netlink/)
- Prior design: Transport enum, OpenHAB Bridge pattern, Home Assistant connectivity model
