# mDNS/DNS-SD discovery

LG webOS TVs also advertise over mDNS/DNS-SD, as Chromecast devices (`_googlecast._tcp.local`). Discovering them that way would sidestep the SSDP firewall problem ([firewall.md](../firewall.md)) on most systems.

## What it would buy

- Port 5353 is usually open already, since Avahi ships on most Linux desktops.
- No new firewall rule on most systems.
- A path to IPv6 discovery: LG TVs don't join the IPv6 SSDP multicast group at all ([lg/firmware-quirks.md § IPv6 SSDP non-participation](../lg/firmware-quirks.md#ipv6-ssdp-non-participation--tvs-are-silent-on-ff02c)), while mDNS's `ff02::fb` is better supported by routers and host firewalls.

## How

- Browse `_googlecast._tcp` and filter by TXT records to pick out LG TVs.
- The [`mdns-sd`](https://crates.io/crates/mdns-sd) crate fits: pure Rust (no Avahi or Bonjour runtime), dual-stack, widely used. `zeroconf` and `astro-dnssd` need the native libraries, which complicates the Nix build; `mdns` is stale; `simple-mdns` and `searchlight` have a smaller install base.
- It would be a second discovery transport with its own socket and firewall story, not a subscriber to the shared SSDP socket ([discovery.md § Shared SSDP socket](../discovery.md#shared-ssdp-socket-concurrent-session-coalescing)).

## Trigger

Users report that discovery fails behind firewalls they can't change, or a backend needs IPv6 discovery.
