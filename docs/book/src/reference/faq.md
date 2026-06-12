# FAQ

### Why control the TV over the network instead of HDMI-CEC?

Many desktop GPUs — notably Nvidia's — expose no HDMI-CEC, so the cable can't carry power/input commands at all. tisvlay talks to the TV's own network API instead: no extra hardware, and it reaches any TV on the LAN, not just the one on the cable. The architecture doesn't preclude CEC/serial/USB transports as future backends.

### Which TVs are supported?

LG webOS TVs today (see [Requirements & Compatibility](./requirements.md)), plus a built-in [mock vendor](../operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend) for testing. The core is vendor-agnostic by design, so additional brands can land as backends.

### Do I need all the services?

No. The daemon plus the CLI is a complete remote control. Add `tisvlay-session` for lock/unlock/idle automation, the tray for menu-bar control, and the GUI for point-and-click setup — each is independent and optional. See [Session Automation](../guide/session-automation.md) for what each service contributes.

### Is `screen off` safe for an OLED?

That's what it's for. `tisvlay screen off` turns the panel off while the TV stays powered and connected — no burn-in exposure, instant resume — which is why it's the default lock/idle action. `power off` is deeper (standby): the TV drops off the network and needs Wake-on-LAN to return.

### Can I script tisvlay?

Yes, two ways: every CLI command takes `--json` for machine-readable output, and the daemon's D-Bus interface (`org.tisvlay.Daemon`) is a stable public API you can drive directly with `busctl` or any D-Bus binding — that's all the bundled clients use. See the [CLI Reference](../guide/cli.md) and the `busctl` examples under [Troubleshooting](../operations/troubleshooting.md#inspecting-the-daemon).

### Where does tisvlay keep its settings?

Three TOML files — the daemon's (TVs + system profiles), your session's (user profiles), and the tray's (notification preferences). Paths and schema in [Configuration](../guide/configuration.md).

### Is the pairing key a secret?

Moderately. The per-TV `client_key` lets any holder on your network control that TV (power, input, volume — it is not a system credential). It lives in the daemon's config at `/var/lib/tisvlay/config.toml`, readable only by the `tisvlay` service user. Treat it like a Wi-Fi password: not catastrophic, not for pasting into bug reports.

### Can I control several TVs?

Yes — pair each one, then target with `-t <tv-id>` (the default is the first configured TV). Automation is per-TV too: each TV carries its own profile assignments and `automate_when` gate, so one TV can blank on lock while another ignores the desktop entirely. See [Profiles](../guide/profiles.md).

---

Something not answered here? [Troubleshooting](../operations/troubleshooting.md) covers the common failure modes (discovery, Wake-on-LAN, pairing), and bug reports are welcome on [GitHub](https://github.com/ruffsl/tisvlay/issues).
