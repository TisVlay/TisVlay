# TVs on an Isolated Network

A smart TV is a computer that phones home: telemetry, ads, viewing data. A common answer is to put it on its own network — an **IoT VLAN** that the router keeps away from the internet and from the rest of your LAN. tisvlay works fine with that setup. It controls the TV entirely over your local network and never needs the TV to reach the internet. But a VLAN boundary breaks the parts of tisvlay that rely on broadcast and multicast, so this page covers what to set up so they keep working.

This applies to TVs controlled over the network. A TV driven over HDMI-CEC talks to tisvlay through the HDMI cable, so which network it's on doesn't matter.

## What crosses the boundary, and what doesn't

| Feature | Traffic | Crosses a router? | What to do |
|---------|---------|-------------------|------------|
| Control (power, screen, input, …) | A unicast connection from tisvlay to the TV | **Yes**, it's ordinary routed traffic | Allow it in the router's firewall |
| Discovery | Multicast search and replies | No | Reflect it, or pair by IP address |
| Wake from standby | Wake-on-LAN broadcast | No | Reflect it |
| MAC auto-fill when you type an IP | The host's ARP cache | No — it only ever sees the router | Read from the TV at pairing, where the backend supports it; otherwise enter it by hand |

The two "reflect it" rows are the ones that need help. A **reflector** on the router copies those specific broadcast and multicast packets from one network to the other, without opening anything else up.

### What each backend uses

The protocols and ports behind those rows depend on the TV's backend:

| Backend | Control | Discovery | Wake | MAC read from the TV |
|---------|---------|-----------|------|----------------------|
| LG webOS | TCP 3001 (TLS); 3000 on older firmware | SSDP | Wake-on-LAN | Yes |
| HDMI-CEC | HDMI cable | Probes the local CEC adapters | Over the HDMI cable | Not needed |

## Router firewall rules

Your router needs to allow the tisvlay host to open connections *to* the TV, while still keeping the TV from reaching the internet or opening connections back into your LAN:

- **Allow** new connections from the tisvlay host (or your LAN) to the TV on its backend's control port (see the table above), plus the return traffic of those connections (established/related).
- **Block** the IoT network from the internet, and from opening new connections into the LAN. That's the point of the VLAN, and tisvlay doesn't need either.

Pairing and every control command are local: the TV never needs internet access for tisvlay to work. (It does for firmware updates and streaming apps — that trade-off is yours.)

## Reflecting discovery and wake with netflector

[netflector](https://github.com/netflector/netflector) is an open-source reflector built for exactly this split: a wired LAN on one side, a Wi-Fi or IoT VLAN on the other. It runs on the router itself or on any machine attached to both networks. It's available for Linux, macOS, and FreeBSD, as a multi-arch Docker image (which also runs as a MikroTik RouterOS container), and as an OPNsense plugin. Its README covers installation and the privileges it needs. It reflects SSDP, mDNS, WS-Discovery and Wake-on-LAN, and can relay other UDP discovery protocols unchanged.

Enable `wol` for wake, plus the discovery protocol your TVs' backends use (the table above). For LG TVs:

```toml
[reflectors.lan_to_iot]
source_if = "br0"   # the network your tisvlay host is on
target_if = "br2"   # the IoT VLAN your TV is on
ssdp = true         # discovery (LG): searches go LAN → IoT, the TV's replies come back
wol  = true         # wake: magic packets go LAN → IoT
# macs = ["84:90:0A:63:A9:94"]   # optional: limit the entry to your TV(s)
```

- **Direction matters.** `source_if` is the side that searches and wakes (your desktop's network); `target_if` is the TV's. netflector relays searches and wakes source → target, and proxies the TV's replies back.
- **Interface names** come from the router: run `ip -br addr` on it and match each network's subnet to its interface. A UniFi gateway, for example, names its default LAN `br0` and each additional network a `br<N>` bridge. Name the per-network interface, not a VLAN trunk — netflector ignores VLAN-tagged frames.
- **`macs`** is optional. It narrows the entry to specific devices; use the MAC of the interface the TV is actually connected through (wired or Wi-Fi) — the one stored for the TV, which `tisvlay config <id>` shows.
- **Other protocols** aren't used by tisvlay unless a backend discovers with them. Enable `mdns` and `dial` too if you want phones on the LAN to cast to the TV.

Many routers ship an mDNS reflector. It only helps backends that discover over mDNS — LG uses SSDP — and it won't relay Wake-on-LAN.

## On the tisvlay side

With the reflector and firewall rules in place, the TV behaves almost like one on your own network:

- **Discovery** finds the TV at its real address on the IoT VLAN. The TV's reply reaches your host through the reflector, so your host firewall rule for discovery replies applies unchanged (for SSDP, the [pinned source port](./firewall-discovery.md#open-the-pinned-source-port)).
- **The MAC** won't be auto-filled when you enter the TV's IP by hand ("MAC not detected"), because the host's ARP cache only sees the router. If the backend can read it from the TV (LG can — see [Pairing → The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac)), leave the field blank; otherwise enter it by hand — it's in the TV's network settings.
- **Wake-on-LAN** works across the boundary. Expect the TV to answer a few seconds after the wake, a little later than one on your own network — tisvlay keeps retrying the connection while it boots, so nothing needs tuning. A TV on Wi-Fi still needs its wake-over-Wi-Fi setting turned on (on LG, **"TV On via Wi-Fi"**).
- **Network awareness** follows the route through the router, so cable pulls and Wi-Fi drops on your side are still noticed straight away (see [Networking & Reliability](./networking-reliability.md)).

## Without a reflector

If you can't run a reflector, tisvlay still works, with two limits:

- **[Pair by IP address](../getting-started/pairing.md#pair-from-the-command-line)**, since discovery can't see the TV.
- **tisvlay can't wake a TV from standby.** Once the TV is powered off, only its remote (or an [HDMI-CEC](../getting-started/pairing.md#hdmi-cec-no-pairing-needed) adapter, if you have one) can bring it back. So change any profile action that powers the TV off to `screen_off`, keeping the TV reachable for the next session; the [stock defaults](../guide/configuration.md#system-profile) show which events do.
