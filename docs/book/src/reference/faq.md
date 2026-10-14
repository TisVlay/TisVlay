# FAQ

### Network or HDMI-CEC — which backend should I use?

For an LG TV, the **network** backend: it needs no extra hardware, reaches any TV on the LAN (not just the one on the cable), and has the fullest feature set. It's also the only option when the GPU has no working CEC (as on Nvidia). **HDMI-CEC** works with any CEC-capable TV regardless of brand, but needs a CEC adapter and covers only power and screen-blank. See [Requirements & Compatibility](./requirements.md).

### Which TVs are supported?

LG webOS TVs over the network, and any CEC-capable TV over HDMI-CEC — see [Requirements & Compatibility](./requirements.md#supported-tvs). There's also a built-in [mock vendor](../operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend) for testing.

### Who can control the TVs?

Whoever is at the machine, automatically; anyone else (SSH, scripts) once they're in the `tisvlay` group. See [Access](../getting-started/installation.md#access).

### Do I need all the services?

No. The daemon plus the CLI is a complete remote control. Add `tisvlay-session` for lock/unlock/idle automation, the tray for menu-bar control, and the GUI for point-and-click setup — each is independent and optional. See [Session Automation](../guide/session-automation.md) for what each service contributes.

### Is `screen off` safe for an OLED?

That's what it's for. `tisvlay screen <id> off` turns the panel off while the TV stays powered and connected — no burn-in exposure, instant resume — which is why it's the default lock/idle action. `power off` is deeper (standby): a network TV then refuses control connections (even if it still answers on the network) and has to be woken to come back.

### Can I script tisvlay?

Yes. Read verbs take `--json` for machine-readable output, and `tisvlay watch` streams every state change without polling. The bundled tray and GUI ride the same interface, so anything they show is scriptable. See the [CLI Reference](../guide/cli.md).

### Where does tisvlay keep its settings?

A few TOML files — the daemon's store (TVs + system profiles) and your per-user session and tray files. Paths and schema in [Configuration](../guide/configuration.md).

### Is the pairing key a secret?

Moderately. An LG TV's pairing key (`client_key`) lets any holder on your network control that TV (power, input, volume — it is not a system credential). It lives in the daemon's store, readable only by the `tisvlay` service user, and the daemon never hands it out: `tisvlay config` and the GUI's configuration view leave it out, and it's redacted from the logs. Treat it like a Wi-Fi password: not catastrophic, not for pasting into bug reports.

### Can I control several TVs?

Yes — pair each one, then target it by its **UUID** (from `tisvlay list`) as the first argument to each verb. Automation is per-TV too: each TV carries its own profile assignments and `automate_when` gate, so one TV can blank on lock while another ignores the desktop entirely. See [Profiles](../guide/profiles.md).

---

Something not answered here? [Troubleshooting](../operations/troubleshooting.md) covers the common failure modes (discovery, Wake-on-LAN, pairing), and bug reports are welcome on [GitHub](https://github.com/TisVlay/TisVlay/issues).
