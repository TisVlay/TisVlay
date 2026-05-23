# SSDP `NOTIFY` passive listener

The streaming discovery story documented in [docs/discovery.md](../discovery.md) has the daemon detect newly-powered-on TVs via a polled `M-SEARCH` loop. Polling at 5-10s feels responsive in practice, but SSDP offers a second message flow we can exploit to drop steady-state network traffic to zero from our side and tighten boot-detection latency to ~1 second.

## What this would add

SSDP devices unsolicited-multicast a `NOTIFY * HTTP/1.1` message to `239.255.255.250:1900` on three lifecycle events:

- `NTS: ssdp:alive` — sent on boot, then periodically (cache-refresh, per the device's `CACHE-CONTROL: max-age` divided by ~2).
- `NTS: ssdp:byebye` — sent on clean shutdown.
- `NTS: ssdp:update` — sent on device-tree changes (rarely used by TVs).

If `tisvlay-ssdp` grows a `Listener` that joins the multicast group and binds to port 1900, the daemon can subscribe to these announcements and react in real time without ever sending an `M-SEARCH`.

## Why it's worth doing even though polling works

Two payoffs that polling can't provide:

1. **Boot-detection latency drops from "up to one poll interval" to ~1s.** A TV powered on at T=0 announces itself within seconds; we forward the event to any active wizard session immediately.
2. **`ssdp:byebye` is a clean power-off signal we currently lack.** Today we infer power-off from carrier/route events and connection drops — a noisy signal that doesn't distinguish "TV powered off cleanly" from "network glitch" from "user pulled the HDMI cable." A `byebye` is the device itself reporting "I'm leaving." This feeds into the broader [event-driven power-management redesign](event-driven-power-management.md) work.

The discovery latency win is the obvious one; the lifecycle win is the load-bearing one.

## Mechanics

- **Multicast group join**: `socket.join_multicast_v4(239.255.255.250, iface)` on a `UdpSocket`. Today's `tisvlay-ssdp` is a pure sender — it binds to an ephemeral port and `send_to`s the multicast address. Receiving unsolicited messages requires explicit group membership.
- **Bind port 1900**: that's where devices send `NOTIFY`. `SO_REUSEADDR` + `SO_REUSEPORT` are required because other system daemons may already hold the port (avahi, gnome's network discovery, KDE's UPnP browser). On bind failure, log and degrade gracefully — polling still works.
- **Per-interface binding**: multicast join is per-NIC. The daemon should join on every network interface that resolves to a configured TV's route (reusing the netlink route-resolution path already in [crates/tisvlayd/src/netlink.rs](../../crates/tisvlayd/src/netlink.rs)), and re-join on NIC up/down events.
- **Message parse**: differs from `M-SEARCH` responses. Start line is `NOTIFY * HTTP/1.1` (not `HTTP/1.1 200 OK`), and the discriminator is the `NTS:` header rather than the response code. The `USN`, `LOCATION`, and `SERVER` headers carry the same content as `M-SEARCH` replies, so vendor identification/parsing (e.g. LG's `is_lg_tv()` + `parse_response()`) can be reused unchanged.

## Composed picture

What this looks like once both layers exist:

- Passive listener runs continuously inside the daemon, joined to the multicast group on every relevant NIC.
- Wizard's "Start discovery" still kicks off one immediate `M-SEARCH` burst — needed to catch TVs that booted *before* the listener started joining (i.e., already-on TVs that aren't due to refresh-announce for several minutes). After the initial burst, it subscribes to the listener's event stream rather than running its own poll loop.
- Steady-state network traffic from `tisvlayd` drops to zero between wizard sessions. The listener is pure receiver.
- Paired-TV power state benefits independently: `byebye` arrivals flow into the same path that handles connection drops, with a tighter "definitely powered off" signal.

## Why it's deferred

- Different socket setup (group join, port 1900 contention) and a parallel parse path doubles the surface area of `tisvlay-ssdp` — worth doing only after the polling-based version has shipped and we've validated which UX gaps remain.
- Latency win is real but small once polling is at 5-10s. The `byebye` lifecycle benefit is what justifies the work, and that's a daemon-wide power-state concern best designed *with* the event-driven power-management redesign rather than in isolation.
- Port 1900 contention with avahi/KDE/gnome on a typical user box needs investigation before we commit to depending on it.

## Triggers to revisit

- Users report the polled wizard feeling sluggish (i.e., 5-10s is too long).
- The event-driven power-management redesign starts and wants a cleaner "TV powered off" signal than carrier/route events provide.
- We add a second vendor and start hitting "N vendors × polling" multicast budgets on enterprise networks.
