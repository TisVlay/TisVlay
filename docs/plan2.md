# Virtual Backlight Kernel Module & Brightness Sync Plan

Sub-plan for Phase 3 of [plan.md](plan.md). Goal: KDE's native brightness slider drives `Tv1.Backlight` on managed TVs with no software dimming — the same experience laptop panels get.

## Status

**Brightness sync is working** via the `kde-external-brightness-v1` Wayland protocol. The forward path (KDE slider → TV backlight) is verified on hardware: moving the KDE brightness slider for "LG TV SSCR2" triggers `requested_brightness` events from KWin, which our session service forwards to the daemon as `set_backlight` calls, and the TV's OLED backlight changes.

**Remaining work**: reverse path (TV remote → KDE slider), feedback-loop suppression, shutdown cleanup, and determining the role of the kernel module vs. pure Wayland approach.

## KDE brightness architecture (learned from source + hardware testing)

### Three layers

**KWin** (the Wayland compositor) is the server of `kde-external-brightness-v1`. It:
- Maintains a list of `ExternalBrightnessDeviceV1` objects registered by clients
- Matches each device to a DRM output by EDID prefix comparison (`workspace.cpp:1498`)
- Sets `BrightnessControl` capability on matched outputs
- Sends `requested_brightness` events when the user moves the slider
- When a brightness device is assigned, disables `allowSdrSoftwareBrightness` for that output (`workspace.cpp:1509-1510`) — **no more software dimming**

**PowerDevil** (power management daemon) is a Wayland client. It:
- Detects brightness-capable displays via three detectors in priority order:
  1. `KWinDisplayDetector` — queries KScreen for outputs with `BrightnessControl` capability
  2. `BacklightDetector` — reads `/sys/class/backlight/` via KAuth privileged helper
  3. `DDCutilDetector` — DDC/CI via libddcutil
- When `ExternalBrightnessController` is active (connected to KWin), diverts non-KWin displays to the Wayland protocol
- Exposes the unified brightness interface at `org.kde.ScreenBrightness` on the session D-Bus

**Our session service** (`tisvlay-session`) is also a Wayland client of the same protocol. We register `kde_external_brightness_device_v1` objects directly with KWin — we don't go through PowerDevil.

### KWin's device-to-output matching (`workspace.cpp:1478-1521`)

```cpp
const auto it = std::ranges::find_if(candidates, [device](BackendOutput *output) {
    if (output->isInternal() != device->isInternal()) return false;  // gate 1
    if (output->isInternal()) return true;                            // internal: first match
    return output->edid().raw().startsWith(device->edidBeginning()); // external: EDID prefix
});
```

For our TV (external HDMI output):
- Gate 1: `output->isInternal()` is `false`, our device sends `set_internal(0)` → passes
- EDID: we send first 128 bytes of the TV's EDID (base64-encoded via `set_edid`), KWin decodes and prefix-matches against the output's full EDID → passes

When matched, KWin assigns the device and **disables software brightness**:
```cpp
changeset->brightnessDevice = device;
changeset->allowSdrSoftwareBrightness = false;
changeset->brightness = device->observedBrightness();
```

### HDR blocker (discovered during testing)

With `highDynamicRange: true` in KWin's output config (`~/.config/kwinoutputconfig.json`), brightness is routed through the HDR tone-mapping path regardless of brightness device assignment:
```cpp
if ((!next.brightnessDevice && next.allowSdrSoftwareBrightness) || effectiveHdr) {
    // software brightness / HDR path — bypasses brightness device
}
```

**Fix**: disable HDR in KDE's display settings for the TV output. Once HDR is off, KWin uses the brightness device for all brightness control. The `requested_brightness` events then flow correctly.

This is acceptable for now: OLED TV backlight control and HDR tone-mapping are fundamentally different operations. A future improvement could handle HDR mode by integrating with KWin's `sdrBrightness` path instead.

## What works today (no kernel module)

The Wayland `kde-external-brightness-v1` protocol alone provides brightness sync when:
1. HDR is disabled for the TV output in KDE settings
2. The session service registers a device with matching EDID and `set_internal(0)`
3. KWin matches the device to the HDMI output and assigns it

This was verified on hardware (LG OLED42C3PUA, Nvidia GPU, KDE Plasma 6.6.3, KWin 6.6.3):
- `busctl --user call ... SetBrightness iu 3000 0` → daemon log shows `D-Bus: set Backlight` → TV backlight changes
- PowerDevil shows the display as `display2` (renumbered from KWin device reassignment)
- MaxBrightness=10000 in PowerDevil's D-Bus (PowerDevil still uses KWinDisplayDetector), but KWin internally routes brightness through our device

## What the kernel module adds (future value)

The kernel module (`tisvlay_backlight`) is **not required** for basic brightness sync but provides:

1. **Preventing software dimming even with HDR**: with a sysfs backlight, `BacklightDetector` can claim the TV, and the external brightness protocol intercepts it — bypassing the KWinDisplayDetector software path entirely.
2. **Session-independent brightness**: the sysfs device persists across login/logout, so brightness state survives session service restarts.
3. **Foundation for DRM power management**: a virtual DRM connector (future scope) could expose the TV with DPMS properties, letting KDE control power state natively.

The module is built and verified:
- Rust + C composite module, builds via Nix against the running kernel (6.18.18, `CONFIG_RUST=y`)
- Creates `/sys/class/backlight/tisvlay-<tv_id>/` with `brightness`, `max_brightness`, `type=platform`
- MiscDevice at `/dev/tisvlay-bl` with ioctl for CREATE/DESTROY/SET_BRIGHTNESS and blocking read for forward events
- PowerDevil's `BacklightDetector` finds the device but can't match it to the HDMI output (`isInternal=true` hardcode + no EDID). This is a PowerDevil limitation, not a module bug — the Wayland protocol path works around it.

## Architecture (current working path)

```
┌──────────┐  requested_brightness  ┌──────────────────────┐
│  KWin    │ ────────────────────>  │  tisvlay-session     │
│  (comp.) │ <────────────────────  │  (Wayland client)    │
│          │  set_observed_bright.  │                      │
└──────────┘                        └──────────┬───────────┘
                                               │ D-Bus
                                    ┌──────────┴───────────┐
                                    │  tisvlayd             │
                                    │  (system daemon)      │
                                    │                       │
                                    │  Tv1.Backlight → TV   │
                                    └───────────────────────┘
```

The kernel module path (future, for HDR + session-independent operation):

```
┌──────────┐  requested_brightness  ┌──────────────────────┐
│  KWin    │ ────────────────────>  │  tisvlay-session     │
│  (comp.) │                        │  (Wayland client)    │
└──────────┘                        └──────────┬───────────┘
                                               │ sysfs write
                                    ┌──────────┴───────────┐
                                    │  /sys/class/backlight │
                                    │  /tisvlay-<tv_id>/    │
                                    └──────────┬───────────┘
                                               │ kernel
                                    ┌──────────┴───────────┐
                                    │  tisvlay_backlight    │
                                    │  /dev/tisvlay-bl      │
                                    └──────────┬───────────┘
                                               │ chardev read
                                    ┌──────────┴───────────┐
                                    │  tisvlayd             │
                                    │  Tv1.Backlight → TV   │
                                    └───────────────────────┘
```

## Dead ends (documented for future reference)

### Attempt 1: kde-external-brightness-v1 alone (commit d0f9a8c, reverted a072773)

Registered a Wayland brightness device without the kernel module. Protocol negotiated v3 successfully, device was created, but `requested_brightness` events never arrived.

**Root cause**: KWin advertised `BrightnessControl` via `allowSdrSoftwareBrightness = true` (software brightness). Our device was registered but KWin's matching silently failed because HDR was enabled — the HDR path bypasses brightness device assignment entirely. We didn't discover the HDR blocker until later.

### Attempt 2: kernel module + BacklightDetector path

Created `/sys/class/backlight/tisvlay-office/` (verified: sysfs attributes correct, brightness=50, max_brightness=100). BacklightDetector found the device via its KAuth helper, but PowerDevil couldn't match it to the HDMI output:
- `BacklightBrightness::isInternal()` is hardcoded to `true` (`backlightbrightness.cpp:274`)
- `DisplayFilter::includes()` rejects `isInternal=true` for an `External` type output (`displaymatch.h:51`)
- `BacklightBrightness` provides no EDID data, so EDID matching can't rescue the failed internal check

### Attempt 3: dB-split volume sync (commit 36dcc50, reverted 2f737f7)

Separate from brightness — attempted to split audio volume between KDE and TV using an α-weighted 60 dB fader. The floating-point rebalancing + u8 quantization on the TV side caused round-trip drift that the suppression cache couldn't absorb, leading to runaway feedback loops. Reverted in favor of 1:1 linear mapping with Device Route writes.

## Kernel module implementation (built, tested, deferred)

### Source: `module/tisvlay-backlight/`

- `tisvlay_backlight.rs` — Rust: MiscDevice registration, ioctl handlers, read path
- `helpers.c` — C: backlight_device_register/unregister, event queue (wait_queue + spinlock), since backlight subsystem and wait queues have no Rust abstractions yet
- `Kbuild` — composite module: `tisvlay_bl-y := tisvlay_backlight.o helpers.o`
- `default.nix` — Nix derivation following `rust-out-of-tree-module` pattern

### Interface

- **Control chardev** (`/dev/tisvlay-bl`):
  - `ioctl(CREATE, { tv_id, max_brightness, initial_brightness })` → creates sysfs backlight device
  - `ioctl(DESTROY, { tv_id })` → removes it
  - `ioctl(SET_BRIGHTNESS, { tv_id, value })` → updates sysfs brightness (reverse path)
  - `read()` → blocks until KDE writes to sysfs (forward path), returns `{ tv_id, brightness }`

### Build requirements

- Linux 6.17+ with `CONFIG_RUST=y` (verified on 6.18.18 with rustc 1.93.0)
- NixOS: `kernel.withRust = true` (already true on unstable nixpkgs)
- Module must be built with the exact rustc version the kernel was compiled with — `kernel.stdenv.mkDerivation` ensures this

## Config surface

```rust
pub enum SyncBacklight {
    None,       // sync disabled, KDE uses software brightness
    Backlight,  // Wayland external brightness device (+ optional kernel module)
}
```

Config value: `sync_backlight = "backlight"`.

**Prerequisite**: HDR must be disabled for the TV output in KDE display settings. Document this in the user-facing setup guide.

## Remaining work

1. **Reverse path** (TV remote changes backlight → KDE slider updates): subscribe to `Tv1.Backlight` PropertiesChanged, call `set_observed_brightness` + `commit` on the Wayland device
2. **Feedback-loop suppression**: last-sent cache to prevent echo (same pattern as volume sync)
3. **Shutdown cleanup**: destroy Wayland devices on exit, fix the blocking dispatch loop for clean Ctrl+C
4. **Scale mapping**: verify `requested_brightness` values map 1:1 to Tv1.Backlight 0-100 (KWin uses our `max_brightness=100`)
5. **HDR documentation**: explain the HDR disable requirement in user docs
6. **Remove WAYLAND_DEBUG**: was temporarily added to tasks.json for protocol tracing (already reverted by user)

## Future scope

- **HDR compatibility**: investigate KWin's `sdrBrightness` path for controlling backlight alongside HDR tone-mapping
- **Virtual DRM connector**: DPMS power management for session-independent TV control
- **Kernel module for software-dimming prevention**: when HDR is enabled, the Wayland protocol alone can't prevent software dimming — the kernel module + upstream KWin changes would be needed
- **PowerDevil BacklightDetector fix**: upstream contribution to make `isInternal()` configurable or EDID-aware for external sysfs backlights
