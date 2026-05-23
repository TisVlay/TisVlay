# Firewall Configuration for SSDP Discovery

The `DiscoverTvs` D-Bus method uses SSDP (Simple Service Discovery Protocol) to find LG TVs on your network. SSDP uses UDP multicast, which may be blocked by strict firewalls.

## How SSDP Works

1. Client sends M-SEARCH to multicast address `239.255.255.250:1900`
2. TVs respond via unicast from their IP, **from a high ephemeral port** (e.g., 50000), **to our ephemeral port**

The key issue: responses don't come from port 1900, so `--sport 1900` rules won't help.

## Symptoms

If discovery returns 0 TVs but the TV is reachable (WebSocket connection works), firewall is likely blocking UDP responses.

## Recommended: pin the SSDP source port

Add to `/var/lib/tisvlay/config.toml` (or `~/.config/tisvlay/config.toml` when running `tisvlayd --session`):

```toml
[discovery]
bind_port = 1901
```

Any unused port above 1024 works. 1901 is a convention — adjacent to SSDP's well-known 1900, not assigned by IANA. Restart `tisvlayd` for the change to take effect.

With a pinned `bind_port`, every distro recipe below collapses to a single-port `--dport` rule:

```bash
# Generic iptables: accept UDP destined for the pinned port
sudo iptables -I INPUT -p udp --dport 1901 -j ACCEPT
```

```nix
# NixOS — declarative form
networking.firewall.allowedUDPPorts = [ 1901 ];
```

NixOS users who manage tisvlayd via the project's NixOS module
([`packaging/nix/module.nix`](../packaging/nix/module.nix)) can do
both the bind-port pinning and the firewall rule in one
declaration — no separate `[discovery]` config edit needed:

```nix
services.tisvlay = {
  enable = true;
  bindPort = 1901;
  openFirewall = true;
};
```

`bindPort` adds `--bind-port 1901` to `tisvlayd`'s `ExecStart` (the
CLI flag overrides `[discovery] bind_port` from the config file, so
the daemon's writable config stays usable for the pairing wizard).
`openFirewall = true` then adds the port to
`networking.firewall.allowedUDPPorts`. The module asserts the two
together — `openFirewall` without `bindPort` set is rejected at
evaluation time.

```bash
# ufw (Ubuntu/Debian)
sudo ufw allow 1901/udp

# firewalld (Fedora)
sudo firewall-cmd --permanent --add-port=1901/udp
sudo firewall-cmd --reload
```

Without `bind_port`, the OS hands out an ephemeral port per scan, so no `--dport` rule can target it — the recipes below all fall back to allowing any UDP from the subnet, which is broader than necessary.

## Solutions by Distribution

These accept any UDP from the local subnet — the fallback for users who have not pinned `bind_port` above. If you pinned a port, use the single-port rules instead.

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

The firewall issue is intrinsic to SSDP on Linux regardless of who runs the multicast — stateful firewalls treat the unicast TV reply as an unrelated new connection (see "Why conntrack Doesn't Help" below). Linux doesn't grant any process a "this is the system discovery daemon, exempt it" carve-out, which is how the other platforms hide the problem.

### Windows
Windows ships a built-in UPnP/SSDP system service ("Function Discovery"). It runs in a privileged path with its own firewall exemption, and apps query already-discovered devices through Device Manager APIs (`SetupDiGetClassDevs()`) — never touching the network themselves. The firewall problem still exists at the protocol level; Windows just exempts the system service from it.

### Android
Android has no firewall by default. Apps can send/receive UDP multicast freely.

### Linux
Linux *has* attempted system-wide SSDP daemons of its own — every attempt either died or never covered our case:

- **Cagibi** (KDE, 2010-2017) — a system-bus SSDP cache/proxy daemon, backing `kio_network` and `kio-upnp-ms` in KDE SC 4.x. Exactly the right shape. Dropped during the KF5/Plasma 5 transition; repo now lives in [KDE's "unmaintained" group](https://invent.kde.org/unmaintained/cagibi) (last commit 2017). Plasma 6 ships nothing equivalent.
- **dleyna** (GNOME, [gitlab.gnome.org/World/dLeyna](https://gitlab.gnome.org/World/dLeyna)) — alive (0.8.3, Sept 2023) and bus-activated on the session bus. Useful for media browsing, but its Manager filters discovered devices at the C-source level against `urn:schemas-upnp-org:device:MediaServer:` and `MediaRenderer:1` — no API surface to ask for vendor-specific URNs like `urn:lge-com:service:webostv:1`. Also not installed by default on Ubuntu, Arch, NixOS, or Fedora.
- **GUPnP / GSSDP** (GNOME) — libraries, not a daemon. Each app does its own multicast.
- **Avahi** — mDNS/DNS-SD only, not SSDP.
- **freedesktop / systemd / xdg-desktop-portal** — nothing covers SSDP today. An mDNS device-discovery portal is in [open discussion](https://github.com/flatpak/xdg-desktop-portal/discussions/1365); no SSDP equivalent is proposed.

So applications implement SSDP themselves, and the firewall problem surfaces as a per-app concern. `tisvlay` ships its own transport in the [`tisvlay-ssdp`](../crates/tisvlay-ssdp/) workspace crate, with `bind_port` configurable specifically so a firewall rule can target a known port (see [the section above](#recommended-pin-the-ssdp-source-port)).

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

## Implementation

`tisvlay` ships its own SSDP transport in the [`tisvlay-ssdp`](../crates/tisvlay-ssdp/) workspace crate — a focused implementation that supports source-port pinning (the missing knob in third-party crates like [`ssdp-client`](https://crates.io/crates/ssdp-client), which always binds an ephemeral port). The LG backend calls it from [`crates/tisvlay-lg/src/discovery.rs`](../crates/tisvlay-lg/src/discovery.rs); future vendor backends share the same transport.

A planned follow-up adds a passive `NOTIFY` listener so the daemon notices TV power-on events in real time (no polling) and picks up clean `ssdp:byebye` power-off signals — see [`docs/ideas/ssdp-notify-passive-listener.md`](ideas/ssdp-notify-passive-listener.md). That work doesn't change the firewall picture (it adds receive-only behavior on the same multicast group), but it reduces how often the daemon needs to send M-SEARCH bursts in steady state.

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
- Use `avahi-browse -r _googlecast._tcp` to discover Chromecast-compatible devices.
- Filter results by TXT records to identify LG TVs specifically.
- Use the [`mdns-sd`](https://crates.io/crates/mdns-sd) Rust crate for programmatic access — pure Rust (no Avahi/Bonjour C runtime dependency), dual-stack v4/v6, ~300K monthly downloads. Picked over `zeroconf` / `astro-dnssd` (Avahi/Bonjour native deps, complicates NixOS packaging), `mdns` (stale, last release 2021), `simple-mdns` / `searchlight` (smaller install base).

mDNS would also be the natural fit for a future v6-capable discovery layer — LG TVs don't join the v6 SSDP multicast group at all ([docs/lg/firmware-quirks.md § IPv6 SSDP non-participation](lg/firmware-quirks.md#ipv6-ssdp-non-participation--tvs-are-silent-on-ff02c) has the capture protocol and the wire evidence), and mDNS's IPv6 multicast (`ff02::fb`) is better-supported by both routers and OS firewalls than SSDP's `ff02::c` regardless. SSAP control over v6 works today; only discovery is the v4-locked path.

**TODO**: Investigate mDNS discovery as an alternative or fallback to SSDP. This would improve out-of-the-box compatibility on systems with strict firewalls.
