# Firewall & Discovery

tisvlay discovers TVs with SSDP (the same multicast protocol DLNA/UPnP use): it sends an `M-SEARCH` to the multicast group and listens for replies.

## The Linux gotcha

LG TVs reply to SSDP from an **ephemeral** source port rather than the standard port 1900. On Linux, a default-deny firewall therefore drops the reply (the kernel has no conntrack entry that matches it), so discovery silently finds nothing — even though the TV is reachable and you can pair it by explicit IP.

**Symptom:** `tisvlay-gui`'s discovery (or `Manager1.StartDiscovery`) returns no TVs, but `tisvlay pair <ip>` works.

## Recommended: pin the source port

Set a fixed SSDP source port in the daemon config so a **single** firewall rule suffices instead of opening the whole ephemeral range:

```toml
# /var/lib/tisvlay/config.toml
[discovery]
bind_port = 1901
```

Then allow inbound UDP on that port:

```sh
# Ubuntu / Debian (ufw)
sudo ufw allow 1901/udp

# Fedora (firewalld)
sudo firewall-cmd --permanent --add-port=1901/udp
sudo firewall-cmd --reload

# Generic iptables
sudo iptables -I INPUT -p udp --dport 1901 -j ACCEPT
```

```nix
# NixOS — declarative
networking.firewall.allowedUDPPorts = [ 1901 ];
```

> Note: a `--sport 1900` rule does **not** work — the TV's reply comes from an ephemeral port, so you must match on the pinned *destination* port instead.

## Alternative: allow UDP from your LAN

If you'd rather not pin a port, allow UDP from your local subnet:

```sh
# ufw
sudo ufw allow from 192.168.1.0/24 proto udp

# firewalld (rich rule)
sudo firewall-cmd --permanent --add-rich-rule='rule family="ipv4" source address="192.168.1.0/24" protocol value="udp" accept'
sudo firewall-cmd --reload
```

Discovery is a convenience — you can always pair by IP address with `tisvlay pair <ip> --vendor lg`, no firewall changes required.
