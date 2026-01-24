# Firewall Configuration for SSDP Discovery

The `DiscoverTvs` D-Bus method uses SSDP (Simple Service Discovery Protocol) to find LG TVs on your network. SSDP uses UDP multicast, which may be blocked by strict firewalls.

## How SSDP Works

1. Client sends M-SEARCH to multicast address `239.255.255.250:1900`
2. TVs respond via unicast from their IP, **from a high ephemeral port** (e.g., 50000), **to our ephemeral port**

The key issue: responses don't come from port 1900, so `--sport 1900` rules won't help.

## Symptoms

If discovery returns 0 TVs but the TV is reachable (WebSocket connection works), firewall is likely blocking UDP responses.

## Solutions by Distribution

### NixOS

NixOS has a strict declarative firewall. Add to your `configuration.nix`:

```nix
# Allow UDP from local network (required for LG TV discovery)
# Note: --sport 1900 doesn't work because LG TVs respond from ephemeral ports
networking.firewall.extraCommands = ''
  iptables -I nixos-fw -p udp -s 192.168.1.0/24 -j nixos-fw-accept
'';
```

**Critical**: Use `-I` (insert) not `-A` (append). The NixOS firewall chain ends with a drop-all rule (`nixos-fw-log-refuse`), so appended rules never match. Also use `nixos-fw-accept` as the target to follow NixOS conventions.

For temporary testing:
```bash
# Enable (insert at top of chain)
sudo iptables -I nixos-fw -p udp -s 192.168.1.0/24 -j nixos-fw-accept

# Disable (remove rule)
sudo iptables -D nixos-fw -p udp -s 192.168.1.0/24 -j nixos-fw-accept
```

### Ubuntu/Debian (ufw)

UFW is typically disabled by default. If enabled:

```bash
# Allow UDP from local network
sudo ufw allow from 192.168.1.0/24 proto udp
```

### Fedora (firewalld)

```bash
# Allow SSDP responses (add to trusted zone or create rich rule)
sudo firewall-cmd --permanent --add-rich-rule='rule family="ipv4" source address="192.168.1.0/24" protocol value="udp" accept'
sudo firewall-cmd --reload
```

### Arch Linux

No firewall by default. If using iptables:

```bash
sudo iptables -A INPUT -p udp -s 192.168.1.0/24 -j ACCEPT
```

## Why This Is a Linux-Specific Problem

### Windows
Windows has a built-in UPnP/SSDP system service ("Function Discovery") that:
- Continuously discovers devices in the background
- Handles all UDP multicast and firewall traversal internally
- Registers discovered devices in Device Manager
- Apps like LGTVCompanion just query Device Manager via `SetupDiGetClassDevs()` - no direct network access needed

### Android
Android has no firewall by default. Apps can send/receive UDP multicast freely.

### Linux
Linux has **no equivalent system-wide UPnP service**. Options:
- **Avahi** - mDNS/DNS-SD only (`.local` names), not SSDP/UPnP
- **gupnp/gssdp** - Libraries for apps, but no system daemon
- **minidlna, Rygel** - DLNA servers, not discovery clients

Applications must implement SSDP discovery themselves, which exposes firewall issues.

### Why conntrack Doesn't Help

Linux firewalls use connection tracking (`ctstate RELATED,ESTABLISHED`) to allow responses. This doesn't work for SSDP because:
- We send to multicast `239.255.255.250:1900`
- Response comes from TV's **unicast IP** (different source address than the multicast we sent to)
- conntrack sees this as a new unrelated connection

**Note**: While SSDP spec says responses should come from port 1900, **LG TVs respond from ephemeral ports** (e.g., 37564, 38678). This means `--sport 1900` won't catch LG TV responses. You need to allow UDP from the local subnet or specific TV IPs.

### Packet Capture Evidence

Packet captures confirm LG TVs violate the SSDP specification by responding from ephemeral ports:

| Device | IP | Source Port | Type |
|--------|---------|-------------|------|
| LG TV | 192.168.1.116 | 35606, 37564, 38678, 54560 | Ephemeral (non-standard) |
| LG TV | 192.168.1.170 | 35608, 37566, 38680, 54558 | Ephemeral (non-standard) |
| Router | 192.168.1.1 | 1900 | Standard |
| Other UPnP | 192.168.1.78 | 1900 | Standard |
| Other UPnP | 192.168.1.95 | 1900 | Standard |

Standard UPnP devices respond from port 1900 as per spec. LG TVs consistently respond from high ephemeral ports in the 35000-55000 range. This behavior is reproducible across multiple discovery attempts and different TV models.

## Why Other Projects Don't Address This

Most LG TV control projects (aiowebostv, bscpylgtv, lgpowercontrol) require manual IP configuration and don't implement SSDP discovery. The few that do (LGWebOSRemote) document that discovery may not work on restricted platforms.

Most desktop Linux distributions ship with no firewall or permissive defaults, so this issue primarily affects:
- NixOS (strict by design)
- Hardened systems with custom firewall rules
- Container/VM environments

## Potential Upstream Improvement

The `ssdp-client` crate binds to an ephemeral port (OS-assigned). If it allowed binding to a **fixed port** (e.g., 1901), we could use a tighter firewall rule:

```bash
# This would work IF ssdp-client bound to port 1901
iptables -I nixos-fw -p udp --dport 1901 -j nixos-fw-accept
```

This would be more restrictive than allowing all UDP from the subnet. A PR to `ssdp-client` to add a `bind_port` parameter could enable this.

## Alternative: Skip Discovery

If you can't modify firewall rules, you can skip discovery and configure TVs manually by IP address using the `PairTv` D-Bus method with a known IP.

## Future Alternative: mDNS/DNS-SD Discovery

LG WebOS TVs also advertise via mDNS/DNS-SD (Chromecast protocol) as `_googlecast._tcp.local`. This could provide a more firewall-friendly discovery method:

**Potential benefits:**
- Port 5353 is usually already open (Avahi is installed on most Linux desktops)
- Works through the system mDNS daemon - no raw socket access needed
- No additional firewall configuration required on most systems
- Avahi handles multicast subscription and response handling

**Implementation options:**
- Use `avahi-browse -r _googlecast._tcp` to discover Chromecast-compatible devices
- Filter results by TXT records to identify LG TVs specifically
- Use `zeroconf` or `mdns` Rust crates for programmatic access

**TODO**: Investigate mDNS discovery as an alternative or fallback to SSDP. This would improve out-of-the-box compatibility on systems with strict firewalls.
