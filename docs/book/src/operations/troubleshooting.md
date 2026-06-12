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

## Isolating tisvlay from the TV (mock backend)

When you can't tell whether a problem is in tisvlay or in the TV / network, take the TV out of the equation: the shipped daemon includes a **"Mock (testing)"** vendor — an in-memory fake TV that pairs, connects, and answers every command without any hardware or network traffic.

Add one from the GUI: **Add TV → manual entry**, vendor *Mock (testing)*, any address (e.g. `mock://test`); leave *Pairing behavior* at `accept` and pair. Without the GUI, add it to the daemon config by hand and reload:

```toml
[[tv]]
uuid = "7d2f9e4a-5b1c-4f6e-9a3d-2e8b4c6f1a05"   # any UUID (uuidgen)
name = "mock-test"
vendor = "mock"

[tv.mock]
address = "mock://test"
```

```sh
sudo busctl call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 ReloadConfig
```

Then exercise it exactly like a real TV — grab its ID from `tisvlay list`, then `tisvlay -t <id> status`, `volume 50`, `screen off`, the tray submenu, the GUI's live controls:

- **The mock works, your TV doesn't** → the tisvlay stack (daemon, D-Bus, services, clients) is healthy; the problem is TV-side or network-side — work through the sections above (WoL settings, firewall, pairing).
- **The mock misbehaves too** → it's a tisvlay bug; please report it with the daemon logs (see [Inspecting the daemon](#inspecting-the-daemon)).

Clean up with `tisvlay remove <id>` when done.
