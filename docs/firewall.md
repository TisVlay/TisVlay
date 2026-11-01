# Why SSDP discovery needs a firewall rule

LG TVs are discovered over SSDP, and a default-deny Linux firewall drops their replies. The rules that let them through are in the user guide ([Firewall & Discovery](book/src/operations/firewall-discovery.md)); our side of the design, a pinned source port, is in [discovery.md](discovery.md). This doc records the network evidence behind both.

## How the replies arrive

1. TisVlay sends an M-SEARCH to the multicast group `239.255.255.250:1900`.
2. Each TV replies by unicast, from its own IP, **from a high ephemeral port**, to our source port. The daemon pins that source port to 1901 by default (`--ssdp-port`), so a single destination-port rule matches every reply.

A `--sport 1900` rule doesn't help: the replies don't come from 1900.

## Why conntrack doesn't help

Stateful firewalls let replies in by connection tracking (`ctstate RELATED,ESTABLISHED`). That doesn't match SSDP:

- the request goes to a multicast address;
- the reply comes from the TV's unicast address, a different source than the one we sent to;
- so conntrack sees the reply as a new, unrelated flow, and a default-deny policy drops it.

## Packet capture evidence

The SSDP specification says replies come from port 1900. LG TVs don't follow it:

| Device | IP | Source port | Type |
|--------|---------|-------------|------|
| LG TV | 192.168.1.116 | 35606, 37564, 38678, 54560 | Ephemeral (non-standard) |
| LG TV | 192.168.1.170 | 35608, 37566, 38680, 54558 | Ephemeral (non-standard) |
| Router | 192.168.1.1 | 1900 | Standard |
| Other UPnP | 192.168.1.78 | 1900 | Standard |
| Other UPnP | 192.168.1.95 | 1900 | Standard |

The LG replies came from ports in the 35000–55000 range on every discovery attempt and on both models tested.

## Why it's a Linux problem

The protocol problem exists everywhere; other platforms hide it behind a system discovery service with its own firewall exemption.

- **Windows** ships a privileged UPnP/SSDP service ("Function Discovery"), exempt from the firewall, and apps query devices it already found through Device Manager APIs (`SetupDiGetClassDevs()`) rather than touching the network.
- **Android** has no firewall by default; apps send and receive UDP multicast freely.
- **Linux** has no live equivalent:
  - **Cagibi** (KDE, 2010–2017) was a system-bus SSDP cache and proxy, exactly the right shape. It was dropped in the move to KF5 and Plasma 5 and now sits in [KDE's unmaintained group](https://invent.kde.org/unmaintained/cagibi).
  - **dleyna** ([GNOME](https://gitlab.gnome.org/World/dLeyna)) is alive and bus-activated, but its Manager filters devices at the C-source level to `MediaServer` and `MediaRenderer:1`, with no way to ask for vendor URNs like `urn:lge-com:service:webostv:1`. It isn't installed by default on the major distros either.
  - **GUPnP / GSSDP** are libraries, not a daemon: each app does its own multicast.
  - **Avahi** is mDNS/DNS-SD only.
  - **freedesktop, systemd and xdg-desktop-portal** have nothing for SSDP. An mDNS device-discovery portal is [under discussion](https://github.com/flatpak/xdg-desktop-portal/discussions/1365).

So every Linux application does its own SSDP, and the firewall problem is each application's to solve. A system daemon doing the multicast would hit the same reply drop anyway.

## Why other projects don't hit it

Most LG TV control projects (aiowebostv, bscpylgtv, lgpowercontrol) take a manually entered IP and don't discover at all. The few that do (LGWebOSRemote) note that discovery may fail on restricted platforms. And most desktop distributions ship no firewall or a permissive one, so the problem mainly shows up on:

- NixOS, strict by design;
- hardened systems with custom rules;
- containers and VMs.

## What's next

A passive `NOTIFY` listener ([ideas/ssdp-notify-passive-listener.md](ideas/ssdp-notify-passive-listener.md)) would only receive on the same multicast group, so it doesn't change this picture. mDNS discovery, which usually needs no new rule, is filed as [ideas/mdns-discovery.md](ideas/mdns-discovery.md).
