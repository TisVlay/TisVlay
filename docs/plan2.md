# Virtual Backlight Kernel Module Plan

Sub-plan for Phase 3 of [plan.md](plan.md). Creates `/sys/class/backlight/tisvlay-*` devices so KDE (and any DE) sees managed TVs as hardware-backlight displays — no software dimming, full bidirectional sync with `Tv1.Backlight`.

## Why a kernel module?

KDE Plasma's brightness system has three layers. Understanding how they interact — and where they fail for HDMI TVs — explains why a kernel module is needed.

### KDE brightness architecture (learned from hardware testing)

**PowerDevil** (the power management daemon) detects brightness-capable displays via three detectors, in priority order:

1. **KWinDisplayDetector** — software brightness via compositor (MaxBrightness=10000, always available for any KScreen output with `BrightnessControl` capability)
2. **BacklightDetector** — hardware backlight via `/sys/class/backlight/`, using a privileged KAuth helper (`org.kde.powerdevil.backlighthelper`) for sysfs access
3. **DDCutilDetector** — hardware DDC/CI via libddcutil

**PowerDevil is a Wayland client** of KWin's `kde-external-brightness-v1` protocol. When the `ExternalBrightnessController` is active (connected to KWin's Wayland global), non-KWin displays are diverted: instead of PowerDevil controlling them directly, it registers them with KWin as `kde_external_brightness_device_v1` objects. KWin then sends `requested_brightness` events back to PowerDevil when the user moves the slider.

The critical routing decision (`screenbrightnesscontroller.cpp:143`):
```cpp
const bool shouldUseExternalControl = m_externalBrightnessController->isActive()
    && !dynamic_cast<KWinDisplayDetector *>(detectorInfo.detector);
```
KWinDisplayDetector displays are **never** sent to external control.

### Why the TV gets software brightness

Our TV has no hardware brightness path:
- No `/sys/class/backlight/` entry (not a laptop panel)
- No DDC/CI (`ddcutil` confirms "I2C slave address x37 is unresponsive" on the LG OLED42C3PUA)

So KWinDisplayDetector claims it with software brightness (MaxBrightness=10000). Neither the `kde-external-brightness-v1` Wayland protocol nor any BacklightDetector path can reach it.

### What a virtual backlight device provides

Creating `/sys/class/backlight/tisvlay-*` gives BacklightDetector a device to find. However, **two additional blockers** were discovered during implementation:

1. **`isInternal` mismatch**: `BacklightBrightness::isInternal()` is hardcoded to `true` (`backlightbrightness.cpp:274`). When PowerDevil tries to match the display to a KScreen output, the `DisplayFilter` requires `isInternal` to match the output type. Our HDMI TV is `External`, not `Panel` — the match fails silently.

2. **No EDID data**: `BacklightBrightness` doesn't provide EDID data. Even if the `isInternal` filter passed, KWin couldn't correlate the device to a specific output without EDID.

3. **KWin priority**: Even with a sysfs backlight present, KWin may still advertise `BrightnessControl` for the HDMI output. KWinDisplayDetector runs first and claims it, making BacklightDetector's finding moot.

### The real fix: KWin integration

The correct solution is for **KWin itself** to use our sysfs backlight for the HDMI output — the same way it uses sysfs backlights for laptop panels. When KWin owns the association, it sets `BrightnessControl` with the real max_brightness (100), applies no software dimming, and routes slider writes directly to sysfs. PowerDevil then sees a KWin-managed display with hardware brightness, not software brightness.

This requires understanding how KWin discovers and associates sysfs backlights with DRM outputs — research ongoing in the KWin source.

### Prior art

- **`ddcci-backlight`** — out-of-tree kernel module, creates sysfs backlight devices for DDC/CI monitors. Well-maintained, packaged in NixOS (`linuxPackages.ddcci-driver`). Our module follows the same registration pattern but with a userspace control channel instead of I2C.
- **Laptop backlight drivers** (`acpi_video`, `apple_bl`, `nvidia_bl`) — each registers a `backlight_device` with the kernel's backlight class; KWin discovers them via sysfs topology and associates them with the internal panel's DRM connector.

## Prior art

- **`ddcci-backlight`** — out-of-tree kernel module, creates sysfs backlight devices for DDC/CI monitors. Well-maintained, packaged in NixOS (`linuxPackages.ddcci-driver`). Our module follows the same registration pattern but with a userspace control channel instead of I2C.
- **Laptop backlight drivers** (`acpi_video`, `apple_bl`, `nvidia_bl`) — each registers a `backlight_device` with the kernel's backlight class; KDE discovers them via sysfs.

## Architecture

The goal is for KWin to use our sysfs backlight as the hardware brightness backend for the HDMI output — the same way it uses sysfs backlights for laptop panels. The architecture has two layers:

**Kernel layer**: the `tisvlay_backlight` module creates sysfs backlight devices and a miscdevice chardev for daemon communication.

**Userspace layer**: `tisvlayd` manages device lifecycle via the chardev; KWin (ideally) or PowerDevil writes to sysfs when the user moves the brightness slider.

```
┌──────────┐  brightness  ┌──────────────────────┐
│  KWin    │  slider      │  /sys/class/backlight │
│  (ideal) │ ──────────>  │  /tisvlay-<tv_id>/    │
│  or      │ <──────────  │  brightness           │
│ PowerDevil  sysfs read  │                       │
└──────────┘              └──────────┬────────────┘
                                     │ kernel
                          ┌──────────┴────────────┐
                          │  tisvlay_backlight     │
                          │  kernel module         │
                          │                        │
                          │  /dev/tisvlay-bl       │
                          │  (misc chardev)        │
                          └──────────┬────────────┘
                                     │ ioctl / read
                          ┌──────────┴────────────┐
                          │  tisvlayd              │
                          │  (system daemon)       │
                          │                        │
                          │  Tv1.Backlight → TV    │
                          └───────────────────────┘
```

**Open question**: how does KWin discover and associate sysfs backlights with DRM outputs? Laptop panels work because the backlight device sits in the same sysfs device tree as the DRM connector. Our platform device has no such parent — we need to understand KWin's association logic (research ongoing) and either:
- Create the correct sysfs topology so KWin finds it naturally
- Or extend KWin's matching (upstream contribution)

## Kernel module: `tisvlay_backlight`

Small out-of-tree GPL module. Registers with the Linux backlight class (`backlight_device_register`).

### Interface

- **Control chardev** (`/dev/tisvlay-bl`): the daemon opens this to create/destroy backlight devices and to receive brightness-write notifications.
  - `ioctl(CREATE, { tv_id, max_brightness })` → creates `/sys/class/backlight/tisvlay-<tv_id>/` with the given max
  - `ioctl(DESTROY, { tv_id })` → removes the backlight device
  - `ioctl(SET_BRIGHTNESS, { tv_id, value })` → updates the `brightness` sysfs attribute (reverse path: TV → KDE)
  - `read()` → blocks until a brightness write from userspace (forward path: KDE → daemon), returns `{ tv_id, value }`

- **Sysfs attributes** (standard backlight class, per device):
  - `brightness` — read/write, 0 to `max_brightness`
  - `max_brightness` — read-only, set at creation time (100 for our TVs)
  - `actual_brightness` — read-only, mirrors `brightness`
  - `type` — `"platform"` (matches `ddcci-backlight` convention)

### Backlight ops

```c
static const struct backlight_ops tisvlay_bl_ops = {
    .update_status = tisvlay_bl_update_status,  // KDE wrote brightness
    .get_brightness = tisvlay_bl_get_brightness, // KDE reads current value
};
```

`update_status` queues the new value for the daemon to read from the chardev. `get_brightness` returns the last value set by either KDE or the daemon.

### Output association

Laptop backlights work because the sysfs backlight device sits in the same device tree as the DRM connector — KWin walks the parent chain and finds the GPU/connector that owns the backlight. Our platform device has no such parent, so KWin can't associate it automatically.

Additionally, PowerDevil's `BacklightDetector` hardcodes `isInternal() = true` for all sysfs backlights and doesn't provide EDID data. Even when the external brightness protocol diverts the device, KWin can't match it to the HDMI output.

**Resolution depends on KWin research** (see "Architecture" open question above). Possible approaches:
1. **Sysfs topology**: register the backlight device as a child of the DRM connector's device node, so KWin's parent-walk finds it naturally. Requires the kernel module to receive the connector's `struct device *` at creation time.
2. **KWin backlight matching**: if KWin supports additional matching heuristics (device symlink, EDID attribute, naming convention), use those.
3. **Upstream contribution**: if no existing mechanism works, contribute a KWin patch that checks for a `drm_connector` sysfs attribute on backlight devices — similar to how `ddcci-backlight` works with PowerDevil today.

## Daemon integration (`tisvlayd`)

The daemon owns the chardev connection. New code in `tisvlayd`:

- **Module loader**: on startup, check if the module is loaded (`/sys/module/tisvlay_backlight/`). If not and the binary is available, `modprobe tisvlay_backlight`. Not fatal if unavailable — brightness sync just won't work.
- **Chardev reader task**: opens `/dev/tisvlay-bl`, spawns a blocking reader. Each message is a brightness write from KDE → forward to `Tv1.Backlight` via the existing connection pool.
- **Device lifecycle**: on TV connect (if `sync_backlight` is enabled + EDID/connector are known), issue `CREATE` ioctl. On disconnect or profile flip to `"none"`, issue `DESTROY`.
- **Reverse path**: on `Tv1.Backlight` property change from the backend (TV remote changed brightness), issue `SET_BRIGHTNESS` ioctl so KDE's slider updates.

This lives in `tisvlayd` (not `tisvlay-session`) because:
- The module needs root-capable `modprobe` and chardev access → system daemon scope
- Backlight devices should persist across user sessions (login/logout shouldn't flicker the TV)
- The session service still owns the *decision* (which TVs get sync, based on resolved profiles) and pushes that via `DesktopMapping`

## NixOS integration

```nix
# packaging/nix/module.nix additions
boot.extraModulePackages = [ pkgs.tisvlay-backlight ];
services.udev.extraRules = ''
  KERNEL=="tisvlay-bl", MODE="0660", GROUP="tisvlay"
'';
```

The module builds against the running kernel via `pkgs.linuxPackages.callPackage`. The udev rule grants the daemon's group access to the chardev without running as root.

## Config surface

The `SyncBacklight` enum is updated (Wayland variant removed, no backward compat):

```rust
pub enum SyncBacklight {
    None,       // sync disabled, KDE uses software brightness
    Backlight,  // virtual sysfs backlight via kernel module
}
```

Config value: `sync_backlight = "backlight"`.

## Implementation language: Rust

The module will be written in Rust, not C. Linux 6.18+ has stable `CONFIG_RUST` support (Rust-for-Linux graduated from experimental in 2025). Key considerations:

- **No upstream Rust backlight bindings yet** — we build thin unsafe wrappers around the C backlight API (`backlight_device_register`, `backlight_ops`). This is ~50 lines of unsafe glue; the rest of the module is safe Rust.
- **NixOS support**: out-of-tree Rust kernel modules are well-supported. The `kernel-module-flake` template and `linuxPackages.callPackage` handle the build. Requires `CONFIG_RUST=y` in the kernel config.
- **Trade-off**: For a ~200-line module, Rust's compile-time lock and error-path safety outweighs the abstraction-building cost. Kernel code handles concurrent access from sysfs writes + ioctl calls; Rust prevents the common lock-ordering and use-after-free bugs that plague small C drivers.

## Scope and ordering

1. **Kernel module** (`module/tisvlay-backlight/`): Rust, Makefile, NixOS package derivation. ~200 lines of Rust + ~50 lines unsafe C FFI glue.
2. **Daemon chardev integration** (`tisvlayd`): new module `backlight_driver.rs`. ~300 lines.
3. **Session service**: no new code — the decision to enable sync is already pushed via `DesktopMapping`. The daemon handles everything.
4. **NixOS module**: `extraModulePackages` + udev rule + `CONFIG_RUST=y` kernel config. ~15 lines of Nix.

## Future scope: power management via virtual DRM connector

A backlight device only controls brightness. A future extension — a virtual DRM connector (VKMS-style) — could expose the TV as a first-class DRM output with DPMS power management, letting KDE control TV power natively (screen off/on via DPMS properties) without the session service's Wayland idle/D-Bus event pipeline. This would:

- Let the DE manage TV power even when `tisvlay-session` isn't running (pre-login, crashed session, SSH-only)
- Unify power management with how laptop panels and real monitors work
- Potentially simplify the session service (less event-driven automation, more DE-native behavior)

However, a virtual DRM connector is significantly more complex than a backlight device (full DRM driver vs. simple sysfs class device). Defer until the backlight module is proven.

Audio sync stays in PipeWire — a kernel module can't improve the slider experience. The cubic gain issue was a PipeWire/KDE volume scale concern, and the current 1:1 mirror approach with Device Route writes is the correct solution.

## Implementation notes

- **Connector path at create time**: the daemon needs the full DRM connector sysfs path to create the symlink for KDE output matching. The session service already knows this (Phase 1 desktop probe → `[tv.desktop].connector`). The daemon receives it via `DesktopMapping`. The full sysfs path (e.g., `/sys/class/drm/card1-HDMI-A-1`) must be passed rather than derived, because multi-GPU setups have varying `card` numbers. The desktop probe already walks `/sys/class/drm/` and can resolve the full path.
- **Module signing**: NixOS doesn't enforce module signing by default, but Secure Boot setups do. Document as a known limitation; flag for later if users report issues.
- **Backlight device naming**: `tisvlay-<tv_id>` requires sanitization — tv_id can contain arbitrary characters from user config. The ioctl handler should restrict to `[a-z0-9_-]` and reject or sanitize invalid names.
- **`CONFIG_RUST` availability**: the NixOS module should check at build time whether the kernel has `CONFIG_RUST=y` and emit a clear error if not, rather than failing with opaque build errors.
