# FAQ

### Network or HDMI-CEC — which backend should I use?

Both ship today. **LG WebOS over the network** needs no extra hardware, reaches any TV on the LAN (not just the one on the cable), and exposes the fullest feature set — power, screen-blank, input, volume, and backlight. It's the natural choice for an LG TV, and the only option where the GPU has no working CEC (as on Nvidia). **HDMI-CEC** drives the TV over the cable through the Linux CEC subsystem, so it works with any CEC-capable TV regardless of brand — but it needs a CEC adapter (a USB CEC dongle, or an amdgpu/i915 DisplayPort→HDMI adapter; most GPU HDMI ports, Nvidia's included, don't wire CEC), and it covers power plus screen-blank (emulated by powering the TV, since CEC can't blank a panel that stays on). Volume, backlight, and arbitrary input switching aren't reliable over CEC. See [Requirements & Compatibility](./requirements.md).

### Which TVs are supported?

LG webOS TVs over the network, and any TV reachable over **HDMI-CEC** (power and screen-blank) with a CEC adapter — see [Requirements & Compatibility](./requirements.md). There's also a built-in [mock vendor](../operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend) for testing. The core is vendor- and transport-agnostic by design, so additional brands can land as backends.

### Who can control the TVs?

Whoever is **at the machine** — the daemon grants the active seat's logged-in user socket access automatically, the same presence model Linux uses for sound cards and cameras, so a fresh install works with zero account setup. Anyone else (SSH, scripts, a second account controlling TVs while not at the console) needs the durable grant: membership in the `tisvlay` group, added one-click by `tisvlay enable-access` (or declaratively via `services.tisvlay.users` on NixOS). The login screen's greeter client has its own scoped grant. And with fast user switching, only the *active* session's automation drives the TVs — a backgrounded account can't blank the panel out from under the person at the machine.

### Do I need all the services?

No. The daemon plus the CLI is a complete remote control. Add `tisvlay-session` for lock/unlock/idle automation, the tray for menu-bar control, and the GUI for point-and-click setup — each is independent and optional. See [Session Automation](../guide/session-automation.md) for what each service contributes.

### Is `screen off` safe for an OLED?

That's what it's for. `tisvlay screen <id> off` turns the panel off while the TV stays powered and connected — no burn-in exposure, instant resume — which is why it's the default lock/idle action. `power off` is deeper (standby): the TV drops off the network and needs Wake-on-LAN to return.

### Can I script tisvlay?

Yes. Every CLI command takes `--json` for machine-readable output, and `tisvlay watch` streams every state key (history first, then live changes) — the event-stream surface for reacting to state without polling. The bundled tray and GUI ride the same interface, so anything they show is scriptable. See the [CLI Reference](../guide/cli.md) and the `watch` note under [Troubleshooting](../operations/troubleshooting.md#inspecting-the-daemon).

### Where does tisvlay keep its settings?

A few TOML files — the daemon's `daemon.toml` (TVs + system profiles), your session's `session.toml` (user profiles), and the tray's `tray.toml` (notification preferences). Paths and schema in [Configuration](../guide/configuration.md).

### Is the pairing key a secret?

Moderately. The per-TV `client_key` lets any holder on your network control that TV (power, input, volume — it is not a system credential). It lives in the daemon's store at `/var/lib/tisvlay/daemon.toml`, readable only by the `tisvlay` service user. Treat it like a Wi-Fi password: not catastrophic, not for pasting into bug reports.

### Can I control several TVs?

Yes — pair each one, then target it by its **UUID** (from `tisvlay list`) as the first argument to each verb. Automation is per-TV too: each TV carries its own profile assignments and `automate_when` gate, so one TV can blank on lock while another ignores the desktop entirely. See [Profiles](../guide/profiles.md).

---

Something not answered here? [Troubleshooting](../operations/troubleshooting.md) covers the common failure modes (discovery, Wake-on-LAN, pairing), and bug reports are welcome on [GitHub](https://github.com/ruffsl/tisvlay/issues).
