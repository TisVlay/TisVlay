# WoL via raw socket (CAP_NET_RAW)

The daemon's WoL story keeps `WakeOptions.broadcast_addr: Ipv4Addr` because IEEE Wake-on-LAN has no IPv6 protocol-level equivalent — v4 magic packet via UDP broadcast remains the only standardized wake path. This works for the common home-LAN case (TV reachable via v4 broadcast on the daemon's local subnet), but degrades in a few specific topologies where granting tisvlayd `CAP_NET_RAW` would unlock more flexible packet construction.

## What it would add

`CAP_NET_RAW` lets the daemon construct raw L2/L3 frames, bypassing the kernel's IP routing logic. Specific WoL gains:

- **Cross-VLAN wake** — craft a packet bound to a specific destination MAC + outbound interface without IP-layer addressing. The kernel's routing table doesn't need to know about the TV's subnet.
- **VPN / tunnel default-route topologies** — when the host's default route is via a VPN, magic packet via the kernel's v4 broadcast goes the wrong way. Raw socket lets us pick the correct outbound interface explicitly.
- **IPv6-only LAN as a special case** — no v4 broadcast address exists; raw L2 frame is the only available mechanism.

**Honest framing**: this doesn't unlock "v6 WoL." The wire format is still classic v4 magic packet over Ethernet broadcast. It unlocks WoL on more network topologies, including v6-only LANs as one specific case.

## Why deferred

`CAP_NET_RAW` is a real privilege escalation:

- The daemon could craft arbitrary network packets (ARP injection, DHCP poisoning, raw TCP sequence manipulation). Far broader than "magic packets."
- The NixOS hardened systemd unit has `CapabilityBoundingSet = [ "" ]` explicitly ([packaging/nix/module.nix:256-257](../../packaging/nix/module.nix#L256-L257)). Granting `CAP_NET_RAW` reverts a deliberate hardening decision.
- For non-Nix distros, the equivalent is `setcap cap_net_raw+ep` on the daemon binary — also a deliberate ops decision.
- Most users on dual-stack home LANs work fine with classic v4 broadcast WoL. The narrow use case doesn't justify expanding privilege by default.

## NixOS module option shape (when implemented)

Follow Tailscale's `useRoutingFeatures` pattern — enum-gated optional capability ([nixos/modules/services/networking/tailscale.nix:13-22](https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/services/networking/tailscale.nix)):

```nix
services.tisvlay.enableWolRawSocket = mkOption {
  type = types.bool;
  default = false;
  description = ''
    Grant CAP_NET_RAW to tisvlayd so Wake-on-LAN can craft L2
    magic packets directly, enabling wake-up on network topologies
    the kernel's routing table doesn't cover: VPN/tunnel default
    routes, cross-VLAN setups, and IPv6-only LANs where there's
    no v4 broadcast address.

    Trade-off: CAP_NET_RAW lets tisvlayd construct arbitrary
    network packets (ARP injection, DHCP poisoning, raw TCP).
    Default false; opt in only if your TV's wake-up doesn't
    work without it.
  '';
};
```

When `true`, the NixOS module's systemd unit extends `AmbientCapabilities = [ "CAP_NET_RAW" ]` and removes `CAP_NET_RAW` from the `CapabilityBoundingSet`'s deny list.

Rust side: runtime capability check (try-the-raw-socket-fall-back-to-broadcast) so the daemon works correctly with or without the capability granted. The same binary ships both paths; the systemd unit configures which is reachable.

For non-NixOS distros, the equivalent ops setup is `setcap cap_net_raw+ep $(which tisvlayd)` — same runtime detection, same fallback semantics.

## Triggers to revisit

- User reports WoL fails on their NixOS + VPN setup.
- User reports IPv6-only LAN can't wake their TV (once IPv6 control support ships).
- Cross-VLAN home setups become more common (smart-home segregation, IoT VLANs).
- Hardened-host operator wants WoL to work without disabling other capabilities.

Until one of these fires, classic v4 broadcast WoL is the right scope.

## Out of scope (don't conflate)

- **mDNS as discovery fallback** (separately filed under [docs/firewall.md](../firewall.md) "Future Alternative: mDNS/DNS-SD Discovery") — different protocol layer.
- **IPv6 socket support for TV control** — covered in [docs/plans/ipv6-support.md](../plans/ipv6-support.md). v6 reaches the TV for control / discovery; WoL is its own concern.
- **True wake-on-MAC without ANY network address** — would require a packet-driver / vendor-specific magic-token protocol. Different problem entirely.
- **Sending the v4 magic packet from a dual-stack host with v4 secondary configured** — doesn't need `CAP_NET_RAW` at all; just bind the existing UDP socket to the right interface. If this turns out to be sufficient for the cross-VLAN / VPN cases, the raw-socket work might not be needed at all.
