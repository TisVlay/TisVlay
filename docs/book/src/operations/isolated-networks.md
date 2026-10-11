# TVs on an Isolated Network

A smart TV is a computer that phones home: telemetry, ads, viewing data. A common answer is to put it on its own network — an **IoT VLAN** that the router keeps away from the internet and from the rest of your LAN. tisvlay works fine with that setup. It controls the TV entirely over your local network and never needs the TV to reach the internet. But a VLAN boundary breaks the parts of tisvlay that rely on broadcast and multicast, so this page covers what to set up so they keep working.

## What crosses the boundary, and what doesn't

| Feature | Traffic | Crosses a router? | What to do |
|---------|---------|-------------------|------------|
| Control — power off, screen, input, volume, backlight | TCP to the TV (port 3001, or 3000 on older LG firmware) | **Yes**, it's ordinary unicast | Allow it in the router's firewall |
| Discovery | SSDP multicast (`239.255.255.250:1900`) | No | Reflect it, or pair by IP address |
| Wake from standby | Wake-on-LAN broadcast (UDP 9) | No | Reflect it |
| MAC auto-fill when you type an IP | The host's ARP cache | No — it only ever sees the router | Nothing: the MAC is read from the TV at pairing |
| HDMI-CEC | The HDMI cable | Not a network feature | Unaffected |

The two "reflect it" rows are the ones that need help. A **reflector** on the router copies those specific broadcast and multicast packets from one network to the other, without opening anything else up.

## Router firewall rules

Your router needs to allow the tisvlay host to open connections *to* the TV, while still keeping the TV from reaching the internet or opening connections back into your LAN:

- **Allow** new connections from the tisvlay host (or your LAN) to the TV on **TCP 3001** (and 3000 if you have older LG firmware), plus the return traffic of those connections (established/related).
- **Block** the IoT network from the internet, and from opening new connections into the LAN. That's the point of the VLAN, and tisvlay doesn't need either.

Pairing and every control command are local: the TV never needs internet access for tisvlay to work. (It does for firmware updates and streaming apps — that trade-off is yours.)

## Reflecting discovery and wake with netflector

[netflector](https://github.com/netflector/netflector) is an open-source reflector built for exactly this split: a wired LAN on one side, a Wi-Fi or IoT VLAN on the other. It runs on the router itself or on any machine attached to both networks. It's available for Linux, macOS, and FreeBSD, as a multi-arch Docker image (which also runs as a MikroTik RouterOS container), and as an OPNsense plugin. Its README covers installation and the privileges it needs.

tisvlay needs two of its protocols: `ssdp` for discovery and `wol` for wake. A minimal configuration:

```toml
[reflectors.lan_to_iot]
source_if = "br0"   # the network your tisvlay host is on
target_if = "br2"   # the IoT VLAN your TV is on
ssdp = true         # discovery: searches go LAN → IoT, the TV's replies come back
wol  = true         # wake: magic packets go LAN → IoT
# macs = ["84:90:0A:63:A9:94"]   # optional: limit the entry to your TV(s)
```

- **Direction matters.** `source_if` is the side that searches and wakes (your desktop's network); `target_if` is the TV's. netflector relays searches and wakes source → target, and proxies the TV's replies back.
- **Interface names** come from the router: run `ip -br addr` on it and match each network's subnet to its interface. A UniFi gateway, for example, names its default LAN `br0` and each additional network a `br<N>` bridge. Name the per-network interface, not a VLAN trunk — netflector ignores frames tagged for another VLAN.
- **`macs`** is optional. It narrows the entry to specific devices; use the MAC of the interface the TV is actually connected through (wired or Wi-Fi). That's the MAC tisvlay reports when you pair (see [Pairing → The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac)).
- **`mdns`, `dial`, and `wsd`** aren't used by tisvlay. Enable `mdns` and `dial` too if you want phones on the LAN to cast to the TV.

Many routers ship an mDNS reflector, but tisvlay doesn't use mDNS — it needs SSDP and Wake-on-LAN, which those built-in reflectors usually don't relay.

## On the tisvlay side

With the reflector and firewall rules in place, the TV behaves almost like one on your own network:

- **Discovery** finds the TV at its real address on the IoT VLAN. The TV's reply reaches your host through the reflector, so the usual [host firewall rule](./firewall-discovery.md#open-the-pinned-source-port) for SSDP's pinned port applies unchanged.
- **The MAC** won't be auto-filled when you enter the TV's IP by hand ("MAC not detected"), because the host's ARP cache only sees the router. Leave the field blank: tisvlay reads the MAC from the TV once pairing completes and saves it.
- **Wake-on-LAN** works across the boundary. Expect the TV to answer a few seconds after the wake, a little later than one on your own network — tisvlay keeps retrying the connection while it boots, so nothing needs tuning. A TV on Wi-Fi still needs **"TV On via Wi-Fi"** enabled in its network settings.
- **Network awareness** follows the route through the router, so cable pulls and Wi-Fi drops on your side are still noticed straight away (see [Networking & Reliability](./networking-reliability.md)).

## Without a reflector

If you can't run a reflector, tisvlay still works, with two limits:

- **Pair by IP address**, since discovery can't see the TV: `tisvlay pair "Living Room" --vendor lg --params '{"ip":"192.168.4.170"}'`.
- **tisvlay can't wake a TV from standby.** Once the TV is powered off, only its remote (or an [HDMI-CEC](../getting-started/pairing.md#hdmi-cec-no-pairing-needed) adapter, if you have one) can bring it back. The default lock and idle actions turn the screen off rather than powering the TV down, so day-to-day automation is unaffected. The system profile's `on_shutdown` and `on_sleep`, though, default to `power_off`; change those (and any other action that powers the TV off) to `screen_off`, so the TV is still reachable when the machine comes back. See [Profiles](../guide/profiles.md).
