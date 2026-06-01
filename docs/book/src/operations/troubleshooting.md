# Troubleshooting

## Discovery finds no TVs

Almost always a firewall dropping the SSDP reply. See [Firewall & Discovery](./firewall-discovery.md). You can pair by explicit IP regardless: `tisvlay pair <ip> --vendor lg`.

## The TV won't power on

Powering on uses Wake-on-LAN, which needs:

- A **MAC address** stored for the TV (pair with `--wake-id <mac>`, or check `tisvlay list`).
- **"TV On via Wi-Fi" / "Mobile TV On"** enabled in the TV's network settings.
- The TV and host on the same L2 network (WoL magic packets don't cross routers).

There is also a 1–5 second delay between the magic packet and the first command while the TV boots, and TVs may ignore WoL during a pixel-refresh cycle.

## Pairing prompt never appears

- Confirm the TV is on and reachable (`ping <ip>`).
- Newer LG firmware requires `wss://` (port 3001) with a self-signed certificate — tisvlay handles this, but a proxy or TLS-inspecting firewall can interfere.
- If credentials previously expired, use `tisvlay re-pair <tv-id>` rather than pairing fresh.

## Commands fail with "not connected"

The daemon reconnects automatically, but a TV drops its WebSocket on standby. Power it on first (`tisvlay power on`), then retry. `tisvlay status` shows the current connection state.

## Inspecting the daemon

```sh
# Daemon logs
journalctl -u tisvlayd -f
# Session service logs (per-user)
journalctl --user -u tisvlay-session -f

# List the daemon's objects and live state in one call
busctl --user call org.tisvlay.Daemon /org/tisvlay \
  org.freedesktop.DBus.ObjectManager GetManagedObjects

# Watch state-change signals
busctl --user monitor org.tisvlay.Daemon
```

(Use the system bus — drop `--user` — when `tisvlayd` runs as a system service.)

## Automation does nothing

- The TV must be `managed = true` (see `tisvlay manage`).
- Check the TV's `automate_when` gate and `primary_input` — with `on_primary`, automation only fires when the TV is showing the PC.
- The per-user `tisvlay-session` service must be running for lock/idle events: `systemctl --user status tisvlay-session`.
