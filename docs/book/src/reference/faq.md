# FAQ

### Network or HDMI-CEC: which should I use?

For an LG TV, the **network** backend: it needs no extra hardware, reaches any TV on your network (not just the one on the cable), and has the fullest feature set. It's also the only option when your GPU has no working CEC (as on Nvidia). **HDMI-CEC** works with any CEC-capable TV regardless of brand, but needs a CEC adapter and covers only power and screen-blank. See [Requirements & Compatibility](./requirements.md).

### Which TVs are supported?

LG webOS TVs over the network, and any CEC-capable TV over HDMI-CEC; see [Requirements & Compatibility](./requirements.md#supported-tvs). There's also a built-in [mock vendor](../operations/troubleshooting.md#isolating-tisvlay-from-the-tv-mock-backend) for testing.

### Who can control the TVs?

Whoever is sitting at the computer, automatically. Anyone else (SSH, scripts) needs to be granted access once; the app offers an **Enable TV control** button, and [Access](../getting-started/installation.md#access) covers the rest.

### Do I need all the parts?

No. The background service plus the app is a complete setup. The session service adds lock, unlock, and idle automation; the tray adds a menu and notifications; the command line adds scripting. Each is independent. See [GUI & Tray](../guide/gui-tray.md) and [Automation](../guide/automation.md).

### Is turning the screen off safe for an OLED?

That's what it's for. The **Screen** switch (or **Screen Off** in the tray) turns the panel off while the TV stays powered and connected, so there's no burn-in exposure and it resumes instantly. That's why it's the default lock and idle action. **Power off** is deeper (standby): a network TV then refuses control connections (even if it still answers on the network) and has to be woken to come back.

### Can I script TisVlay?

Yes. The `tisvlay` command has `--json` output for its read commands, and `tisvlay watch` streams every state change without polling. The app and tray use the same interface, so anything they show is scriptable. See the [Command Line](../guide/cli.md) page.

### Where does TisVlay keep its settings?

In a few TOML files: the service's store (TVs and system profiles) and your own per-user session and tray files. You normally never touch them, since the app edits them for you. Paths and schema are in [Configuration Files](../guide/configuration.md).

### Is the pairing key a secret?

Moderately. An LG TV's pairing key (`client_key`) lets any holder on your network control that TV (power, input, volume; it is not a system credential). It's stored where only the `tisvlay` service user can read it, and the service never hands it out: the app's CONFIG view and `tisvlay config` leave it out, and it's redacted from logs. Treat it like a Wi-Fi password: not catastrophic, but don't paste it into bug reports.

### Can I control several TVs?

Yes. Add each one with the **+** button. Automation is per-TV too: each TV has its own profiles and its own **Managed** switch, so one TV can blank when you lock while another ignores your desktop entirely. See [Automation](../guide/automation.md).

---

Something not answered here? [Troubleshooting](../operations/troubleshooting.md) covers the common failure modes (discovery, Wake-on-LAN, pairing), and bug reports are welcome on [GitHub](https://github.com/TisVlay/TisVlay/issues).
