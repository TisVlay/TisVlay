# Firewall & Discovery

When you click **+** to add a TV, the pairing wizard scans your network. Network backends find TVs by multicast discovery: TisVlay sends a search to a multicast group and listens for replies. The LG backend uses SSDP (the same protocol DLNA and UPnP use), which is what this page's firewall rules cover. HDMI-CEC TVs aren't discovered over the network at all; the computer's CEC adapters are probed instead.

Discovery is a convenience. You can always add a TV by typing its address under **Don't see your TV? Add it manually**, with no firewall changes; see [Adding a TV](../getting-started/pairing.md).

## The Linux gotcha

LG TVs reply to SSDP from an **ephemeral** source port rather than the standard port 1900. On Linux, a default-deny firewall therefore drops the reply (the kernel has no conntrack entry that matches it), so discovery silently finds nothing, even though the TV is reachable and you can add it by address.

**Symptom:** the wizard's scan finds no network TVs (or `tisvlay discover` returns none), but adding the TV by address works.

## Open the pinned source port

TisVlay binds its SSDP source socket to a **fixed** port, **1901 by default**, so a single firewall rule lets the replies through instead of opening the whole ephemeral range. You don't need to configure the port; just allow inbound UDP on it:

```sh
# Ubuntu / Debian (ufw)
sudo ufw allow 1901/udp

# Fedora (firewalld)
sudo firewall-cmd --permanent --add-port=1901/udp
sudo firewall-cmd --reload

# Generic iptables
sudo iptables -I INPUT -p udp --dport 1901 -j ACCEPT
```

On NixOS the module opens it for you; its `bindPort` already defaults to 1901:

```nix
services.tisvlay.openFirewall = true;
# or, by hand:
networking.firewall.allowedUDPPorts = [ 1901 ];
```

To use a different port, or an OS-chosen ephemeral one (which can't be narrowed to a single firewall rule), set it in the service's config, or pass `tisvlay-daemon --ssdp-port <port>` (`0` = ephemeral):

```toml
# /var/lib/tisvlay/daemon.toml
[discovery]
bind_port = 1901   # the default; any unused port works, or 0 for ephemeral
```

> Note: a `--sport 1900` rule does **not** work. The TV's reply comes from an ephemeral port, so you must match on the pinned *destination* port instead.

## Alternative: allow UDP from your LAN

If you'd rather not pin a port, allow UDP from your local subnet:

```sh
# ufw
sudo ufw allow from 192.168.1.0/24 proto udp

# firewalld (rich rule)
sudo firewall-cmd --permanent --add-rich-rule='rule family="ipv4" source address="192.168.1.0/24" protocol value="udp" accept'
sudo firewall-cmd --reload
```

## Ways to discover and add a TV

- **The pairing wizard** scans live: TVs appear as they answer, and the host's CEC adapters are listed alongside the network scan.
- **Manual entry** is the fallback when discovery can't reach the TV. Type its address; for an LG TV the wizard fills in the MAC address (used for Wake-on-LAN) from your computer's neighbor cache and marks it with an **Auto-detected** badge, which clears if you edit the value. A TV on another subnet or VLAN isn't visible to that cache; see [The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac) and, for discovery across a VLAN, [TVs on an Isolated Network](./isolated-networks.md).

<details>
<summary>From a terminal</summary>

- `tisvlay discover` runs a bounded scan; found TVs stream back as they answer.
- `tisvlay discover-session` is an open-ended scan that keeps streaming findings until you press ctrl-c.
- `tisvlay pair` adds a TV by address; see [Adding a TV](../getting-started/pairing.md#pair-from-the-command-line).

</details>
