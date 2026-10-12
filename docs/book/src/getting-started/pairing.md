# Pairing

Pairing establishes a trusted connection between tisvlay and your TV. What it involves depends on the backend: an LG webOS TV shows an on-screen prompt the first time, which you accept with the remote, and tisvlay stores the resulting credentials; an HDMI-CEC TV needs no prompt at all ([below](#hdmi-cec-no-pairing-needed)).

## Pair from the command line

```sh
tisvlay pair "<name>" --vendor lg --params '{"ip":"192.168.1.100"}'
```

- `"<name>"` — a friendly label shown in `tisvlay list` and the GUI (the first positional argument).
- `--vendor lg` — the backend to use. `tisvlay vendors` lists the registered backends, and `tisvlay schema <vendor>` shows a backend's pairing fields.
- `--params` — the backend's pairing fields as a JSON object. For LG: `ip` (required) and `mac` (optional; `wake_id` is accepted as an alias) — the MAC address Wake-on-LAN needs to wake the TV from standby. Leave `mac` out and tisvlay reads it from the TV (see [below](#the-wake-on-lan-mac)).

Accept the prompt on the TV when it appears. On success the TV is added and appears in `tisvlay list` with its assigned **UUID** — the id you pass to every control and configuration verb.

Pairing by IP address like this works whether or not [discovery](../operations/firewall-discovery.md) can see the TV.

### The Wake-on-LAN MAC

When pairing completes, tisvlay asks the TV for the MAC address of the network interface it's actually connected through (wired or Wi-Fi) and reports what it did with it on a `mac:` line:

- `mac: detected AA:BB:CC:DD:EE:FF (saved)` — you didn't supply a MAC, so the TV's is stored and Wake-on-LAN works from now on.
- `mac: AA:BB:CC:DD:EE:FF (matches the TV)` — the MAC you supplied is the right one.
- `mac: TV reports …, configured …; to use it: tisvlay config-set <id> '{"mac":"…"}'` — the MAC you supplied differs (often the TV's *other* interface, e.g. the wired MAC on a TV that's on Wi-Fi). Yours is kept; run the printed command to switch to the TV's.

A MAC you supply is never overwritten. This is the reliable way to get a MAC for a TV on a different subnet or VLAN, where the host's ARP cache can't see it. It's an LG feature, and it needs the TV paired by IPv4 address — over IPv6 or a hostname, or if the TV doesn't answer within a few seconds, there's no `mac:` line and you supply the MAC yourself.

Newly paired TVs are **managed** (they take part in automation) by default. To pair one for manual control only, mark it unmanaged afterward: `tisvlay managed <id> false`.

## Pair from the GUI

`tisvlay-gui` includes a pairing wizard that discovers TVs on the network — and lists the host's CEC adapters — then walks through the prompt. Launch it from your application menu (or run `tisvlay-gui`).

The wizard's last step shows the same MAC report: *MAC detected from the TV* when it filled a blank one, or — when the MAC you entered differs from the TV's — both addresses with **Use reported** and **Keep mine** buttons.

## HDMI-CEC: no pairing needed

HDMI-CEC has no credentials and no on-screen prompt — any device on the CEC bus is reachable, so "pairing" a CEC TV just means telling tisvlay which adapter it's on:

- **GUI wizard** — the pairing wizard lists the host's CEC adapters (`/dev/cec0`, `/dev/cec1`, …), probes each for the TV's own name and make/model, and prefills the device. Pick the adapter and confirm.
- **By hand** — declare the TV in the daemon configuration with `vendor = "cec"` and its adapter's `device`; see [Configuration](../guide/configuration.md#tv-definitions).

The TV must be on the CEC bus — powered, or even in standby (the bus reaches a standby TV) — and on the input the adapter is plugged into. See [Installation](./installation.md#hdmi-cec-backend-optional) for adapter requirements.

## Re-pairing

Credentials can expire or be revoked by the TV. Refresh them in place without changing the TV's id or settings:

```sh
tisvlay repair <id>
```

A re-pair reads the TV's MAC too, so it's also how to give an already-paired TV a MAC it's missing.

## Requirements for pairing

- The TV is powered on and reachable on the network. (Answering `ping` isn't enough: a TV in standby can keep its network up while refusing control connections.)
- Newer LG firmware requires a secure (`wss://`) connection with a self-signed certificate — tisvlay handles this automatically.

If the prompt never appears, see [Troubleshooting](../operations/troubleshooting.md#pairing-prompt-never-appears).
