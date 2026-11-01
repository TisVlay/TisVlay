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

## Vendor-OUI branding (SIMPLINK input icon)

Some TVs (LG SIMPLINK among them) key the input's *icon*/branding on the source's reported vendor OUI via `<Give Device Vendor ID>`; a Chromecast reports Google's OUI to get its badge. The same `LinuxCecLink::open`-time hook that advertises the OSD name could call `AsyncDevice::set_vendor_id(Option<VendorId>)` (present in linux-cec 0.2.0, always buffer-only — no transmit path), fed by a `[tv.cec].vendor_oui` field typed as `Option<VendorId>` so the library's serde `Deserialize` (the `"aa-bb-cc"`, exactly-two-hex-digits-per-octet form; `VendorId` also implements `FromStr`) validates at config-parse time rather than failing every connect in the hot path.

Deferred because TisVlay owns no registered OUI — any value it claims is another vendor's identity, impersonation that can also confuse a TV's own SIMPLINK device handling — and because the payoff is unconfirmed (whether the icon actually changes on the C3/G4). Trigger: a field test on the target LGs showing the icon meaningfully improves; ship off-by-default as an expert knob with the impersonation caveat stated plainly. If it lands, fold `osd_name` + `physical_address` + `vendor_id` into one "what the adapter claims" struct on the open seam rather than a third positional parameter.

## Never let the inputattach unit give up while the tty exists

The NixOS module's `pulse8-cec-inputattach@` unit restarts `inputattach` whatever its exit (`Restart=always`, #212) and is bound to the tty device, so a suspend's clean exit and a real unplug are both covered. What's left is systemd's default start limit: more than 5 starts in 10 s marks the unit failed for good, and `/dev/cecN` then stays gone while the adapter is still plugged in, until someone restarts the unit. `StartLimitIntervalSec=0` would make it keep trying for as long as the tty exists; `BindsTo` still stops it when the adapter is unplugged, so it can't loop on a missing node.

Deferred because nothing has hit the limit yet: the two suspends tested (2026-10-24) each needed one restart. Trigger: a journal showing the unit failed with `start-limit-hit`, or an adapter that flaps on resume.

## The Pulse-Eight disarm: upstream, a suspend opt-out, and linux-cec

Our out-of-tree `pulse8-cec` build disarms the dongle on disconnect ([the bug doc](../bugs/cec-pulse8-autonomous-label.md)). What's left of that work:

- **Upstream the patch.** It's Nix-only until it has soaked in use. Then it goes to linux-media with a `Fixes: 323f52e02be6` tag, cc Hans Verkuil and Vicki Pfau, so other distros get it.
- **A suspend opt-out.** The disarm covers reboots and shutdowns, not a suspend: a sleeping adapter runs autonomously by design, so it can still pin the label. An opt-out would be a NixOS option (`cec.pulse8.autonomous`) beside `cec.pulse8.enable`, never a TisVlay profile setting, since it's driver-level, Pulse-Eight-specific and needs root. A sleeping adapter follows the autonomous flag in its EEPROM, not the driver's working state, so the option has to set the EEPROM: libcec 8 writes it (`am 0`), and the driver's `persistent_config=1` makes it follow the claim ([docs/cec/protocol.md](../cec/protocol.md#the-tv-waking-the-pc)). Turning autonomy off also stops the TV from waking the PC.
- **A distro-agnostic fallback.** The daemon could clear its claim on SIGTERM, with its unit ordered `After=` the inputattach unit so it stops first, disarming through a live serio on any distro without the driver patch. Not needed while the patch holds.
- **A linux-cec note upstream.** `all_device_types` is never filled, so a CEC 2.0 `<Report Features>` advertises no device types, and there's no way to set `cec_version` ([the bug doc](../bugs/cec-pulse8-autonomous-label.md), § Related findings). It can go with the pending [OSD-name issue draft](../bugs/upstream-linux-cec-reclaim-drops-osd-name.md).

Trigger: a soak with no label regressions (upstream), a user who'd rather give up the wake than risk the label after a suspend (the opt-out), or the next linux-cec issue filed (the note).
