# Opt-in HDMI-CEC setup for distro packages

NixOS can enable CEC device access for an existing adapter with `services.tisvlay.cec.enable`. For Pulse-Eight, `services.tisvlay.cec.pulse8.enable` also enables device access while building and attaching the USB driver. The `.deb`, `.rpm`, and Arch packages currently require users to add a systemd drop-in and udev rule manually. Pulse-Eight USB adapters have another host-side requirement: the kernel `pulse8-cec` module plus `inputattach --pulse8-cec`, usually started by a udev-triggered systemd unit.

## Goal

Offer an explicit, supported setup path for CEC on distro-package installs without weakening the daemon sandbox for users who do not opt in. Keep GPU DisplayPort CEC and USB serial adapters distinct: the former needs the CEC device access grant; the latter also needs the driver and inputattach binding.

## Candidate path

- Ship a small `tisvlay-cec-setup` helper or optional CEC integration package for `.deb`, `.rpm`, and Arch.
- On explicit enable, install a systemd drop-in granting only `char-cec` access and relaxing `PrivateDevices`, plus a udev rule that gives the `tisvlay` group access to CEC nodes.
- For Pulse-Eight, provide an inputattach service template and matching USB udev rules, and check that the host has the `pulse8-cec` module and `inputattach` utility. Do not attempt to build or replace the host's kernel from the package.
- Make enable/disable idempotent, preserve administrator-owned drop-ins, and remove only files managed by the helper.
- Keep the existing default unit sandbox unchanged until the administrator opts in.

## Trade-offs and validation

An optional package is easy to discover through the package manager but adds distro-specific dependency metadata. A helper avoids another package but needs a privileged installation path and careful rollback behavior. Either approach must avoid clobbering existing systemd overrides.

Validate on Debian and Fedora package installs with both DP-AUX CEC and a Pulse-Eight adapter: check that the expected `/dev/cecN` appears, the daemon can open it after opt-in, unplug/replug reattaches the USB adapter, and disabling the integration restores the original sandbox. Check Arch/CachyOS and Bazzite behavior separately rather than assuming their kernel module or inputattach service is enabled by default.

Deferred until the distro CEC setup is implemented or repeated user reports show the current manual steps are a meaningful blocker.
