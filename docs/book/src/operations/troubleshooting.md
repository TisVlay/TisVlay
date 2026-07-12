# Troubleshooting

## Discovery finds no TVs

Almost always a firewall dropping the SSDP reply. See [Firewall & Discovery](./firewall-discovery.md). You can pair by explicit IP regardless: `tisvlay pair "My TV" --vendor lg --params '{"ip":"192.168.1.100"}'`.

## The TV won't power on

Powering on uses Wake-on-LAN, which needs:

- A **MAC address** stored for the TV — pass it in `--params` when pairing (`"mac":"AA:BB:CC:DD:EE:FF"`); `tisvlay config <id>` shows what's stored.
- **"TV On via Wi-Fi" / "Mobile TV On"** enabled in the TV's network settings.
- The TV and host on the same L2 network (WoL magic packets don't cross routers).

There is also a 1–5 second delay between the magic packet and the first command while the TV boots, and TVs may ignore WoL during a pixel-refresh cycle.

## Pairing prompt never appears

- Confirm the TV is on and reachable (`ping <ip>`).
- Newer LG firmware requires `wss://` (port 3001) with a self-signed certificate — tisvlay handles this, but a proxy or TLS-inspecting firewall can interfere.
- If credentials previously expired, use `tisvlay repair <id>` rather than pairing fresh.

## Commands fail with "not connected"

The daemon reconnects automatically, but a TV drops its connection on standby. Power it on first (`tisvlay power <id> on`), then retry; `tisvlay reconnect <id>` nudges a downed TV to redial. `tisvlay list` shows each TV's current connection state (`tisvlay status` only probes the daemon itself).

## Inspecting the daemon

```sh
# Daemon logs
journalctl -u tisvlay-daemon -f
# Session service logs (per-user)
journalctl --user -u tisvlay-session -f

# Current state of every TV, hydrated from the daemon's caches
tisvlay list

# Stream every data key live (history first, then changes as they happen)
tisvlay watch
```

`tisvlay watch` is the event-stream view — it replaces the old bus-monitor workflow. If even `tisvlay status` fails, the daemon isn't reachable: check that it's running (`systemctl status tisvlay-daemon`) and that **your user is in the `tisvlay` group** (the group guards the daemon's control socket; re-login after adding yourself).

## Automation does nothing

- The TV must be managed: `tisvlay managed <id> true`.
- Check the resolved policy with `tisvlay profile effective <id>` — with `automate_when = on_primary`, automation only fires when the TV is showing the PC (its `primary_input`).
- If you streamed in remotely, wake actions are suppressed on purpose while the TV is armed — see [Remote-Session Awareness](../guide/remote-sessions.md).
- The per-user `tisvlay-session` service must be running for lock/idle events: `systemctl --user status tisvlay-session`.

## Isolating tisvlay from the TV (mock backend)

When you can't tell whether a problem is in tisvlay or in the TV / network, take the TV out of the equation: the shipped daemon includes a **"Mock (testing)"** vendor — an in-memory fake TV that pairs, connects, and answers every command without any hardware or network traffic.

Add one from the GUI (**Add TV → manual entry**, vendor *Mock (testing)*, any address like `mock://test`), or in one command:

```sh
tisvlay add "mock-test" --address mock://test
```

Then exercise it exactly like a real TV — grab its id from `tisvlay list`, then `tisvlay set-volume <id> 50`, `tisvlay screen <id> off`, the tray submenu, the GUI's live controls:

- **The mock works, your TV doesn't** → the tisvlay stack (daemon, services, clients) is healthy; the problem is TV-side or network-side — work through the sections above (WoL settings, firewall, pairing).
- **The mock misbehaves too** → it's a tisvlay bug; please report it with the daemon logs (see [Inspecting the daemon](#inspecting-the-daemon)).

Clean up with `tisvlay remove <id>` when done.
