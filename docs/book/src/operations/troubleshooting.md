# Troubleshooting

## Discovery finds no TVs

Almost always a firewall dropping the SSDP reply. See [Firewall & Discovery](./firewall-discovery.md). You can pair by explicit IP regardless: `tisvlay pair "My TV" --vendor lg --params '{"ip":"192.168.1.100"}'`.

## The TV won't power on

Powering on uses Wake-on-LAN, which needs:

- A **MAC address** stored for the TV — `tisvlay config <id>` shows what's stored. If it's missing, `tisvlay repair <id>` reads it from the TV (LG, paired by IPv4 address); otherwise set it with `tisvlay config-set <id> '{"mac":"AA:BB:CC:DD:EE:FF"}'`.
- The **right** MAC: a TV has separate wired and Wi-Fi MACs, and only the interface it's connected through wakes it. If the pairing's `mac:` line said the TV reports a different MAC than the one configured, switch to the TV's with the `config-set` command it printed. See [Pairing → The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac).
- **"TV On via Wi-Fi" / "Mobile TV On"** enabled in the TV's network settings.
- The TV and host on the same L2 network — WoL magic packets are broadcasts and don't cross routers, unless your router relays or reflects them onto the TV's subnet or VLAN (see [TVs on an Isolated Network](./isolated-networks.md)).

There is also a 1–5 second delay between the magic packet and the first command while the TV boots, and TVs may ignore WoL during a pixel-refresh cycle.

## Pairing prompt never appears

- Confirm the TV is on and reachable (`ping <ip>`). A ping reply alone doesn't prove it's on — a TV in standby can keep its network up while refusing control connections — so check the screen.
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

`tisvlay watch` is the event-stream view — it replaces the old bus-monitor workflow. If even `tisvlay status` fails, the CLI tells you which of the two failure modes you're in:

- **"You don't have TV control access yet"** — an access problem, and the fix is in the message: run `tisvlay enable-access` (one polkit authentication, then log out and back in — a reboot is the most reliable, since the systemd user manager can keep the old groups across a plain logout). You'll typically only see this over SSH or from a service account: the user at the console has access automatically.
- **"daemon unreachable"** — the daemon itself is down or the socket path is wrong: check `systemctl status tisvlay-daemon` and `journalctl -u tisvlay-daemon`.

## No tray icon on GNOME

Vanilla GNOME ships no system-tray host, so `tisvlay-tray` runs but has nowhere to draw its icon. Install the AppIndicator extension (`gnome-shell-extension-appindicator`; Ubuntu bundles its own) and the icon appears. Until then the tray still delivers notifications and serves the GUI's settings — only the icon and its menu are missing. See [GUI & Tray](../guide/gui-tray.md#tray-applet-tisvlay-tray).

## Automation does nothing

- The TV must be managed: `tisvlay managed <id> true`.
- Check the resolved policy with `tisvlay profile effective <id>` — with `automate_when = on_primary`, automation only fires when the TV is showing the PC (its `primary_input`).
- If you streamed in remotely, wake actions are suppressed on purpose while the TV is armed — see [Remote-Session Awareness](../guide/remote-sessions.md).
- If another account is the active session (fast user switching), your backgrounded session's lock/idle automation is suppressed on purpose until you're foreground again.
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
