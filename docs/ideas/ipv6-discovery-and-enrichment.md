# IPv6 discovery and enrichment

The current stack — SSDP search ([crates/tisvlay-ssdp/src/lib.rs](../../crates/tisvlay-ssdp/src/lib.rs)), `arp_lookup` ([crates/tisvlayd/src/netlink.rs](../../crates/tisvlayd/src/netlink.rs)), and the in-flight manual-entry MAC autofill (see [docs/plans/manual-entry-mac-autofill.md](../plans/manual-entry-mac-autofill.md)) — is IPv4-only by deliberate scope. `tisvlay-ssdp::search` returns `Error::Ipv6BindUnsupported` if given a v6 bind address; the manual-entry enrichment RPC will reject IPv6 inputs with an empty map. IPv6 support is a coherent piece of follow-up work that touches the whole discovery + enrichment surface.

## What v1 does in lieu

- `tisvlay-ssdp::SearchOptions::bind_addr` rejects IPv6 explicitly with `Error::Ipv6BindUnsupported`. SSDP discovery is IPv4-only (`239.255.255.250:1900`).
- `Manager1.EnrichManualEntry` gates inputs to `IpAddr::V4` at the daemon entry point — IPv6 addresses (including IPv4-mapped `::ffff:x.x.x.x`) return an empty map alongside other "nothing to do" outcomes (unknown vendor, invalid IP, out-of-subnet).
- Subnet allowlist covers RFC1918 + link-local only — no IPv6 ULA, no IPv6 link-local.
- `arp_lookup` accepts `IpAddr` in its signature but in practice the IPv4 case is the only one exercised; the IPv6 path is dead code today.

## What enabling IPv6 entails

Each layer needs its own piece of work, and the layers compose:

### Neighbour table lookup (not ARP)

IPv6 doesn't use ARP — it uses NDP (Neighbour Discovery Protocol). The kernel maintains a separate neighbour table (`ip -6 neighbour show` vs `ip neighbour show`). Same netlink primitive (`RTM_GETNEIGH`), different `NDA_*` attributes in the response, different protocol semantics. `arp_lookup` would either:

- Rename to `neighbour_lookup` and branch on `ip.is_ipv6()` to issue the right netlink family request, or
- Stay v4-specific and gain a sibling `ndp_lookup` for v6.

The first is cleaner — both protocols ultimately just resolve `IpAddr → MacAddress` from a kernel cache; the protocol distinction is plumbing.

### NDP cache priming via `probe_arp_cache`

The active probe in `probe_arp_cache(ip: IpAddr)` works the same way on IPv6 — `UdpSocket::send_to((v6_addr, port))` triggers an outbound packet, which makes the kernel emit a `NEIGHBOR_SOLICITATION` to resolve the destination. The MAC ends up in the v6 neighbour table as a side effect, same as the v4 case populates the ARP table. The connector impl needs no semantic change beyond a different `UdpSocket::bind` call (`[::]:0` for v6 vs `0.0.0.0:0` for v4) — the trait signature already takes `IpAddr` and is family-agnostic.

The route-resolution helper (`resolve_route_for_ip`, added in the manual-entry MAC autofill PR) already handles v6 routes via the same rtnetlink API — `prefsrc` is family-aware.

### SSDP IPv6 multicast

The harder piece. IPv6 SSDP uses `ff02::c` (link-local scope) for M-SEARCH, with the wrinkle that **link-local IPv6 multicast requires specifying the outgoing interface** at send time — there's no equivalent of v4's "kernel picks the route" behavior. `tisvlay-ssdp::search` would need either:

- Per-interface scoped sockets (iterate interfaces, send one M-SEARCH per interface, merge responses)
- Caller-supplied interface (push the interface-selection responsibility outward)

Both are visible API changes — `SearchOptions` grows an `interface: Option<String>` field, or `search` returns a per-interface stream of streams. Pick the API direction when implementing; both have precedent in other UPnP libraries.

### Subnet allowlist expansion

The manual-entry enrichment's subnet guard widens to include:

- `fc00::/7` — IPv6 ULA (the v6 analogue of RFC1918)
- `fe80::/10` — IPv6 link-local (legitimate for local TVs that don't have a globally-routable address)

The IPv4-mapped-IPv6 case (`::ffff:x.x.x.x`) is worth canonicalizing rather than rejecting at this point — once v6 is supported in earnest, surfacing it as v4 is the friendlier UX.

### LG TV reality check

A v6 implementation lands or doesn't depending on whether LG TVs actually expose meaningful endpoints on v6 in practice. Worth a packet-capture investigation before committing to the work: do they respond to v6 M-SEARCH? Do they accept v6 connections to the SSAP port? Do they advertise v6 addresses in their UPnP descriptions? Today's assumption is "v4 only because that's what's tested" — the answer might be "v4 only because that's what LG ships."

If LG TVs are v4-only on the wire, this work is for future vendors (Samsung, Sony, Roku?) where v6 might matter more.

## Triggers to revisit

- A user reports a v6-only network setup (rare on home LANs, more plausible on enterprise / IPv6-mandate environments).
- A second vendor backend lands whose devices do speak v6.
- The discovery-time experience needs to find devices on v6-only segments (IoT VLANs, guest networks set up v6-only for some reason).
- Packet capture of an LG TV reveals it speaks v6 SSDP / SSAP — the per-vendor case becomes "we're leaving a feature on the table."

Until one of those fires, IPv4-only is the right scope. The v6 work is well-bounded but is real work — probably 200-300 LOC across `tisvlay-ssdp`, `tisvlayd/netlink`, `tisvlayd/dbus/manager`, and the connector trait — not a one-line type change.

## Out-of-scope follow-ons (don't conflate)

- **mDNS / DNS-SD discovery** (separately filed under [docs/firewall.md](../firewall.md)) — also v4 today, also a multicast scope question, but on a different protocol family. v6 mDNS is `ff02::fb` and has the same per-interface scoping issue. Worth tackling together with SSDP v6 once both land on the roadmap.
- **NDP-snooping for power-state inference** (not currently a feature on v4 either) — IPv6's neighbour reachability tracking is richer than v4's; a `STALE`-to-`REACHABLE` transition could signal a TV waking up. Separate idea entirely.
