# Plan: Per-device NM carrier monitoring for accurate disconnect detection

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

### Route-based device resolution

When a network-transport TV connects, the daemon resolves which NM
device carries traffic to its IP via the OS routing table:

```
ip route get 192.168.1.170
→ 192.168.1.170 dev eno2 src 192.168.1.79
```

This maps: TV IP → NIC name → NM device object path.

The daemon doesn't need to ask the user which interface to use —
the kernel routing table already knows. This works correctly with
bridges, VLANs, bonding, VPNs, etc.

### Per-device carrier monitoring

Instead of (or in addition to) NM global `StateChanged`, the watcher
monitors `Carrier` property changes on the specific NM device object
that routes to each TV:

```
/org/freedesktop/NetworkManager/Devices/3
    org.freedesktop.NetworkManager.Device.Wired
        Carrier: true → false (instant on cable pull)
```

Carrier changes fire within milliseconds of physical link up/down.
No connectivity check delay.

### Architectural layers

```
Vendor (LG, Epson, Sony)
    ↓ connects via
Protocol (SSAP, PJLink, HTTP)
    ↓ carried over
Transport (Network, Serial, USB, CEC)  ← TvConnector::transport()
    ↓ routed through
NM Device (eno2, wlan0, br0)           ← route lookup (daemon-owned)
    ↓ signals
Carrier state (up/down)                ← NM Device.Wired.Carrier
```

The backend declares `Transport::Network`. The daemon resolves the
NM device. The NM watcher monitors per-device carrier. This keeps
backends vendor-agnostic and protocol-agnostic — only the daemon
knows about NM devices.

### Where does the route lookup live?

The route lookup is a **daemon** responsibility, not a backend concern:

- The backend provides the TV's address (IP) via `display_address()`
- The daemon resolves IP → NM device at connection time
- The NM watcher uses this mapping to monitor the right device

This is transport-level logic that the daemon owns (same as how the
daemon owns the `reconnecting` flag and NM event scoping).

### State machine per TV

```
Disconnected → [connect] → Connected (resolve NM device, start monitoring)
Connected → [carrier lost on device] → Disconnecting → Disconnected
Connected → [NM global lost] → Disconnecting → Disconnected (fallback)
Disconnected → [carrier restored on device] → Reconnecting → Connected
```

### IP resolution approach

Options for resolving TV IP → NM device:

**A. `ip route get <ip>`** — shell out to iproute2. Simple, always
correct (uses the kernel FIB), but requires spawning a process.

**B. Netlink socket** — query the kernel routing table directly via
rtnetlink. No process spawn, async-friendly. The `rtnetlink` or
`netlink-packet-route` crate can do this.

**C. NM D-Bus** — query NM's `GetDeviceByIpIface` or inspect
`ActiveConnection` objects for the IP range. Stays within D-Bus
but adds NM API coupling.

**D. `libc::getifaddrs` + routing table** — pure Rust, no external
dependencies. More code but fully self-contained.

Recommendation: **A** for simplicity now. It's a one-shot call at
connection time, not a hot path. Can be upgraded to B later if the
process spawn is a concern.

### Multiple TVs on different interfaces

If TV A is on eno2 and TV B is on wlan0, the watcher monitors both
devices. Carrier loss on eno2 disconnects only TV A. This is correct
behavior — a wired disconnect shouldn't affect Wi-Fi TVs.

### Fallback to global state

Keep the existing global `StateChanged` monitoring as a fallback for:
- TVs where route resolution fails (shouldn't happen, but defensive)
- Non-NM systems (the global watcher already degrades gracefully)
- Scenarios where per-device monitoring misses an event

The global watcher remains as the safety net. Per-device carrier is
the primary signal for faster detection.

## Questions to resolve

1. **When to resolve the route?** At connection time (once) or
   periodically? Routes can change (DHCP renewal, failover). Once
   at connection time is probably sufficient — if the route changes,
   the TCP connection would break anyway.

2. **Multiple routes to same TV?** Bonding/ECMP could have multiple
   paths. Monitor all devices in the route? Or just the primary?

3. **Wi-Fi carrier?** NM's `Carrier` property exists on
   `Device.Wired` but not `Device.Wireless`. For Wi-Fi, use
   `Device.Wireless.ActiveAccessPoint` or the device's generic
   `State` property instead.

4. **Should we track the NM device path on ManagedTv?** Adding
   `nm_device: Option<String>` to `ManagedTv` would let
   `disconnect_network_tvs` check per-device carrier instead of
   disconnecting all network TVs.

## Implementation plan

### Phase 1: Route resolution utility

Add a function to resolve TV IP → NIC name:

```rust
/// Resolve which network interface routes to the given IP.
async fn resolve_interface(ip: &str) -> Option<String> {
    // Shell out to: ip route get <ip>
    // Parse: "... dev <ifname> ..."
}
```

### Phase 2: NM device lookup

Map NIC name → NM device object path via NM D-Bus:

```rust
/// Find the NM device object path for a network interface.
async fn nm_device_for_interface(conn: &Connection, ifname: &str) -> Option<OwnedObjectPath> {
    // Call: org.freedesktop.NetworkManager.GetDeviceByIpIface(ifname)
}
```

### Phase 3: Per-device carrier monitoring

Subscribe to `PropertiesChanged` on each NM device object. When
`Carrier` changes to `false`, disconnect the associated TV(s).

### Phase 4: Track NM device on ManagedTv

Add `nm_device: Option<String>` to `ManagedTv`. Set at connection
time via route resolution. Used by disconnect/reconnect to scope
events to the correct device.

### Phase 5: Integrate with existing watcher

Keep global `StateChanged` as fallback. Per-device carrier is the
primary signal. Both paths feed into the same `StateChangeEvent`
channel.

---

## Checklist

- [ ] Phase 1: Route resolution utility
- [ ] Phase 2: NM device lookup
- [ ] Phase 3: Per-device carrier monitoring
- [ ] Phase 4: Track NM device on ManagedTv
- [ ] Phase 5: Integrate with existing watcher
- [ ] Update design.md and CLAUDE.md

---

## References

- [NM Device D-Bus API](https://networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.Device.html)
- [NM Device.Wired](https://networkmanager.dev/docs/api/latest/gdbus-org.freedesktop.NetworkManager.Device.Wired.html) — `Carrier` property
- [rtnetlink crate](https://docs.rs/rtnetlink/) — async Rust netlink
- Current NM watcher: `crates/tisvlayd/src/network_watcher.rs`
