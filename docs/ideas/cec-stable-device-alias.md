# Stable device alias for USB CEC adapters

A CEC TV's config pins a kernel node name (`device = "/dev/cec0"`, which the pairing wizard prefills from discovery). Kernel names are assigned at registration, so they aren't stable: a second adapter, or a re-attached one that comes back while a descriptor on the old node is still open, can register as `/dev/cec1`. The configured path then never becomes available, and the TV waits for a device that's now under another name.

How it surfaced: the device-availability work ([docs/plans/cec-device-presence.md](../plans/cec-device-presence.md), § Renumbering). The kernel frees a node's minor only when the last descriptor on it closes, and that plan drops the handle on the unavailable edge, so our own descriptors no longer hold the old minor. That makes renumbering rare, not impossible: another process holding the node, or two adapters registering in a different order at boot, still renumbers.

## Path forward

- **A udev alias in the NixOS module.** `cec.pulse8.enable` already installs udev rules for the adapter's tty (`packaging/nix/module.nix`). A rule matching the CEC node's parent device (the Pulse-Eight's USB vendor and product IDs, `2548:1001` / `2548:1002`, plus its serial when there's more than one) could add `SYMLINK+="cec-pulse8"` (or `cec-pulse8-<serial>`). The same rule ships for other distros as a documented snippet in [cec-distro-setup.md](cec-distro-setup.md).
- **The wizard prefills the alias when one exists.** Discovery lists `/dev/cecN` nodes; for each, it could check whether a known alias resolves to it and prefill that instead, so new pairings survive renumbering without the user knowing about udev.
- **Existing configs keep working.** A config that names `/dev/cec0` stays valid; the alias is only a better default. The availability watcher already re-checks paths through symlinks once udev has created them.

## Trade-offs

- **Adapter-specific rules.** Each adapter model needs its own match (Pulse-Eight here; DisplayPort-AUX CEC on amdgpu or i915 is a different parent device and rarely renumbers). A generic alias keyed on the connector or the USB path would avoid per-model rules but is harder to read.
- **More than one adapter of a kind** needs the serial in the alias, so the rule depends on the adapter exposing one.

## Trigger

A user reports a CEC TV stuck waiting for its device after a re-attach or a second adapter, or the renumbering log line (from the availability work) shows up in a field report.
