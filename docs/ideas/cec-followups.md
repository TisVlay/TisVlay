# CEC backend follow-ups

Deferred work surfaced while building the HDMI-CEC backend (`tisvlay-cec`, PR #94). Each is a real gap or limitation, kept out of the initial backend to keep its scope to power + active-source input. Grouped here rather than as five separate files because they share a trigger — a user running CEC hardware in a shipped build hits them together.

Backend model + what *did* ship: [docs/cec/protocol.md](../cec/protocol.md).

## Relative volume (cross-cutting capability)

CEC has no absolute volume set — only relative `User Control Pressed{Volume Up/Down}`, and absolute readback (`Report Audio Status`) is an AVR/soundbar feature, device-dependent. The current capability model is absolute-only (`SetVolume y`, `Volume` property), so CEC declares no `VOLUME` and the control is gated off entirely.

A proper fix is a **cross-cutting capability-model extension**, not CEC-local: a relative-volume primitive (`VolumeUp`/`VolumeDown` steps) threaded through the `Capabilities` bit, `TvConnection` methods, the `Tv1.Volume` D-Bus interface, the CLI, and the GUI/tray (step buttons vs. an absolute slider). Its own plan when a relative-only backend (CEC, or an RS-232 amp) makes it worth the surface. Until then CEC simply omits volume.

## Multi-transport single TV (identity correlation)

The same physical panel can be reachable two ways at once — e.g. an LG set over the network (SSAP) *and* over CEC. v1 treats each as an independent config entry with its own UUID; there's no notion that they're one TV.

The hook for unifying them is device identity. CEC folds OSD name + vendor into the cross-vendor `HardwareId` (Make/Model), and LG populates the same from `getSystemInfo` — so a future reconciliation could match entries by `HardwareId` (+ serial where available) and present one TV with a preferred transport. `linux-cec`'s `get_connector_info()` (`DrmConnector { card_no, connector_id }`) is a second correlation signal, but it needs a resolution step (walk `/sys/class/drm/card{card_no}-*`, match the KMS id) to reach the sysfs connector name the daemon's `desktop_probe.rs`/`Tv1.Desktop.Connector` are keyed on, and may be `None` on a USB adapter. Deferred as its own design.

## DPMS-mirror may not engage for a USB-CEC TV

The host-display DPMS watcher ([docs/design.md § Host display DPMS watcher](../design.md#host-display-dpms-watcher)) matches a TV to its *video* connector by EDID, then mirrors that connector's DPMS state onto the TV. A USB-CEC adapter reaches the TV over a **separate `/dev/cecN` cable**, and its own connector info points elsewhere (or is `None`), so the EDID→connector match that drives the mirror can't find the CEC TV. A CEC TV whose video also runs through an EDID-matchable output (e.g. DP→HDMI on amdgpu/i915, where DP-AUX CEC tunnels on the *same* cable) is fine; the gap is specifically the USB-adapter-on-a-separate-cable topology. Resolving it overlaps the multi-transport identity work above (correlate the CEC entry to a video connector) and the [virtual DRM connector](virtual-drm-connector.md) idea. Known limitation, not yet worked.

## GUI: hide the Screen toggle on power-emulated backends

With `emulate_screen_via_power` on, the daemon advertises `SCREEN` for a CEC TV so the stock screen-action profiles work — but that also makes the GUI/tray render a manual **Screen on/off toggle** that just power-cycles, duplicating the Power button. Shipped as-is (the reworded help explains it, and hiding a control whose profile action still fires elsewhere is more surprising than showing it). The polish: hide the *manual* toggle for emulated-screen backends while keeping the capability for profile dispatch — needs a small daemon→GUI "screen is emulated" signal (the daemon knows via the `emulate_screen_via_power` predicate; the client only sees the `SCREEN` bit today).

## Generalize the wizard's network-shaped manual entry

The pairing wizard's manual-add step is network-shaped: the field is labeled "IP" with a `192.168.1.100` placeholder, and autofill seeds only fields named `ip`/`address`. CEC works around it by making discovery non-optional (the `/dev/cecN` device picker is the primary entry path) and naming its field `device` with `autofill = "discovery.device"`. The follow-up is to drive the manual-entry label/placeholder from the **schema field** itself rather than hardcoding "IP", so a chardev (or serial) backend's manual path reads honestly instead of asking for an IP. Small, self-contained; deferred because the device picker already covers the common CEC case.
