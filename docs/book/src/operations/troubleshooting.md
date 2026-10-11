# Troubleshooting

## Discovery finds no TVs

Almost always a host firewall dropping the discovery replies. See [Firewall & Discovery](./firewall-discovery.md). You can [pair by IP address](../getting-started/pairing.md#pair-from-the-command-line) regardless.

## The TV won't power on

An HDMI-CEC TV wakes over the cable; check that it's on the adapter's input and that the daemon can reach the adapter ([Installation](../getting-started/installation.md#hdmi-cec-backend-optional)). A network TV wakes by Wake-on-LAN, which needs:

- A **MAC address** stored for the TV — `tisvlay config <id>` shows what's stored. If it's missing, `tisvlay repair <id>` reads it from the TV where the backend supports that (see [Pairing → The Wake-on-LAN MAC](../getting-started/pairing.md#the-wake-on-lan-mac)); otherwise set it with `tisvlay config-set <id> '{"mac":"AA:BB:CC:DD:EE:FF"}'`.
- The **right** MAC: a TV has separate wired and Wi-Fi MACs, and only the interface it's connected through wakes it. If pairing reported a different MAC than the one configured, switch with the `config-set` command it printed.
- **Wake over the network enabled on the TV** — on LG, **"TV On via Wi-Fi"** (or "Mobile TV On") in its network settings.
- The TV and host on the same L2 network — WoL magic packets are broadcasts and don't cross routers, unless your router relays them onto the TV's subnet or VLAN (see [TVs on an Isolated Network](./isolated-networks.md)).

A woken TV takes a few seconds (typically 1–5) to boot before it accepts commands — tisvlay keeps retrying — and TVs may ignore WoL during a pixel-refresh cycle.

## Pairing prompt never appears

This applies to backends with an on-screen prompt (LG); HDMI-CEC has none.

- Confirm the TV is on and reachable (`ping <ip>`). A ping reply alone doesn't prove it's on — a TV in standby can keep its network up while refusing control connections — so check the screen.
- Newer LG firmware requires `wss://` (port 3001) with a self-signed certificate — tisvlay handles this, but a proxy or TLS-inspecting firewall can interfere.
- If credentials previously expired, use `tisvlay repair <id>` rather than pairing fresh.

## Commands fail with "not connected"

The daemon reconnects automatically, but a network TV drops its connection on standby. Power it on first (`tisvlay power <id> on`), then retry; `tisvlay reconnect <id>` nudges a downed TV to redial. `tisvlay list` shows each TV's current connection state.

## Inspecting the daemon

```sh
journalctl -u tisvlay-daemon -f           # daemon logs
journalctl --user -u tisvlay-session -f   # session service logs (per-user)
tisvlay list                              # every TV's current state
tisvlay watch                             # live state changes
```

If even `tisvlay status` fails, the CLI tells you which of two failure modes you're in:

- **"You don't have TV control access yet"** — an access problem; run `tisvlay enable-access` (see [Access](../getting-started/installation.md#access)). You'll typically only see this over SSH or from a service account.
- **"daemon unreachable"** — the daemon itself is down or the socket path is wrong: check `systemctl status tisvlay-daemon` and `journalctl -u tisvlay-daemon`.

## A hand edit didn't take

- **Reload failed** — the message names the file and why; a parse error shows the line and column. Nothing from that file was applied: fix it and run `tisvlay reload` again.
- **A `<file>.edited` appeared** — you edited a state file but a service wrote it before you reloaded, so your edit was moved aside rather than overwritten. Redo it in the current file and reload.

See [Hand-editing and reloading](../guide/configuration.md#hand-editing-and-reloading).

## No tray icon on GNOME

Vanilla GNOME has no system-tray host; see [GUI & Tray](../guide/gui-tray.md#tray-applet-tisvlay-tray) for the extension that adds one.

## Automation does nothing

- The TV must be managed: `tisvlay managed <id> true`.
- Check the resolved policy with `tisvlay profile effective <id>` — with `automate_when = on_primary`, automation only fires when the TV is showing the PC (its `primary_input`).
- If you streamed in remotely, wake actions are suppressed on purpose while the TV is armed — see [Remote-Session Awareness](../guide/remote-sessions.md).
- If another account is the active session (fast user switching), your backgrounded session's automation is suppressed on purpose until you're foreground again.
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
