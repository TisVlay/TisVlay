# Pairing

Pairing establishes a trusted connection between tisvlay and your TV. For LG WebOS, the TV displays an on-screen prompt the first time — you accept it with the remote, and tisvlay stores the resulting credentials.

## Pair from the command line

```sh
tisvlay pair "<name>" --vendor lg --params '{"ip":"192.168.1.100","mac":"AA:BB:CC:DD:EE:FF"}'
```

- `"<name>"` — a friendly label shown in `tisvlay list` and the GUI (the first positional argument).
- `--vendor lg` — the backend to use. `tisvlay vendors` lists the registered backends, and `tisvlay schema <vendor>` shows a vendor's pairing fields.
- `--params` — the vendor's pairing fields as a JSON object. For LG: `ip` (required) and `mac` (the MAC address for Wake-on-LAN — strongly recommended, since without it the TV cannot be powered on from standby; `wake_id` is accepted as an alias for `mac`).

Accept the prompt on the TV when it appears. On success the TV is added and appears in `tisvlay list` with its assigned **UUID** — the id you pass to every control and configuration verb.

Newly paired TVs are **managed** (they take part in automation) by default. To pair one for manual control only, mark it unmanaged afterward: `tisvlay managed <id> false`.

## Pair from the GUI

`tisvlay-gui` includes a pairing wizard that discovers TVs on the network — and lists the host's CEC adapters — then walks through the prompt. Launch it from your application menu (or run `tisvlay-gui`).

## HDMI-CEC: no pairing needed

HDMI-CEC has no credentials and no on-screen prompt — any device on the CEC bus is reachable, so "pairing" a CEC TV just means telling tisvlay which adapter it's on:

- **GUI wizard** — the pairing wizard lists the host's CEC adapters (`/dev/cec0`, `/dev/cec1`, …), probes each for the TV's own name and make/model, and prefills the device. Pick the adapter and confirm.
- **By hand** — declare the TV in the daemon configuration with a `[tv.cec]` block (at minimum `device = "/dev/cec0"`); see [Configuration](../guide/configuration.md#tv-definitions).

The TV must be on the CEC bus — powered, or even in standby (the bus reaches a standby TV) — and on the input the adapter is plugged into. See [Installation](./installation.md#hdmi-cec-backend-optional) for adapter requirements.

## Re-pairing

Credentials can expire or be revoked by the TV. Refresh them in place without changing the TV's id or settings:

```sh
tisvlay repair <id>
```

## Requirements for pairing

- The TV is powered on and reachable on the network.
- For LG: **"TV On via Wi-Fi"** (or "Mobile TV On") is enabled in the TV's network settings, so Wake-on-LAN works.
- Newer LG firmware requires a secure (`wss://`) connection with a self-signed certificate — tisvlay handles this automatically.

If discovery doesn't find your TV, it's usually a firewall blocking SSDP — see [Firewall & Discovery](../operations/firewall-discovery.md). You can always pair by explicit IP address (in `--params`) regardless of discovery.
