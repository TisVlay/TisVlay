# Virtual Backlight Kernel Module & Brightness Sync Plan

Sub-plan for Phase 3 of [plan.md](plan.md). Goal: KDE's native brightness slider drives `Tv1.Backlight` on managed TVs with no software dimming — the same experience laptop panels get.

## Status

**Brightness sync forward path is working** in both SDR and HDR modes. Verified on hardware (LG OLED42C3PUA, Nvidia GPU, KDE Plasma 6.6.3, KWin 6.6.3, HDR enabled):
- **SDR mode**: KWin routes slider changes through our `kde_external_brightness_device_v1` via `requested_brightness` Wayland events
- **HDR mode**: KWin bypasses the brightness device for tone-mapping, but the `org.kde.ScreenBrightness.Display.Brightness` D-Bus property still changes — a D-Bus `PropertiesChanged` observer catches these and forwards to `Tv1.Backlight`, scaled from 0-10000 to 0-100

Both paths feed into the same drain-loop for coalescing.

**Reverse path** (TV remote → KDE slider) is **not working**. The Wayland `set_observed_brightness` protocol request only sets initial state — KWin's `commit()` handler is a one-shot (`m_done` guard), so subsequent commits are no-ops and KWin never re-reads the observed brightness. This is a protocol design limitation, not a bug. A working reverse path would need to call PowerDevil's `SetBrightness` via D-Bus directly, with feedback-loop suppression to avoid echoing our own forward writes.

**Feedback-loop suppression** implemented for the forward path: the drain-loop records `last_forward_sent` per TV, and the reverse path checks it to suppress echoes from the daemon reflecting our own writes.

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

### HDR mode behavior

With `highDynamicRange: true`, KWin uses the brightness slider for tone-mapping reference luminance and does NOT call `setBrightness` on external brightness devices (`drm_output.cpp:630`):
```cpp
if (m_state.brightnessDevice && !m_state.highDynamicRange) {
    m_state.brightnessDevice->setBrightness(effectiveBrightness);
}
```

So the Wayland `requested_brightness` event never fires in HDR mode. However, the `org.kde.ScreenBrightness.Display.Brightness` D-Bus property still changes (PowerDevil's `KWinDisplayDetector` tracks it). The D-Bus observer in `brightness_sync.rs` subscribes to `PropertiesChanged` on this interface and forwards brightness changes to the TV, scaled from KWin's 0-10000 range to our 0-100 `Tv1.Backlight` range.

Both the Wayland and D-Bus paths feed into the same `BrightnessEvent` channel and drain-loop, so in SDR mode (where both fire) the drain-loop deduplicates naturally.

## What works today (no kernel module)

Two forward paths, no kernel module required:

1. **Wayland path (SDR)**: session service registers a `kde_external_brightness_device_v1` with matching EDID and `set_internal(0)`. KWin matches it to the HDMI output and sends `requested_brightness` events when the slider moves.
2. **D-Bus observer path (HDR)**: subscribes to `PropertiesChanged` on `org.kde.ScreenBrightness.Display` interfaces. When the `Brightness` property changes, resolves the display label to a TV ID via a cached EDID monitor-name → tv_id map (rebuilt on each reconcile), scales 0-10000 → 0-100, and forwards to the drain-loop.

Both paths feed into the same `BrightnessEvent` channel → drain-loop → `DaemonClient::set_backlight`.

The D-Bus observer caches display object path → label lookups to avoid repeated D-Bus round-trips. The label → tv_id map is maintained by reconcile from the EDID monitor name read at binding time.

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
                  SDR: requested_brightness
┌──────────┐  ───────────────────────────>  ┌───────────────────┐
│  KWin    │  <───────────────────────────  │  tisvlay-session  │
│  (comp.) │  SDR: set_observed_brightness  │                   │
└────┬─────┘                                │  Wayland client + │
     │ KScreen                              │  D-Bus observer   │
┌────┴─────┐  HDR: PropertiesChanged        │                   │
│PowerDevil│  ───────────────────────────>  │                   │
│  (D-Bus) │                                └─────────┬─────────┘
└──────────┘                                          │ D-Bus
                                            ┌─────────┴─────────┐
                                            │  tisvlayd          │
                                            │  Tv1.Backlight→TV  │
                                            └───────────────────┘
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

## SDR mode findings

Verified: KWin still reports MaxBrightness=10000 in SDR mode (PowerDevil's KWinDisplayDetector claims the display). Our brightness device is registered and matched by KWin (EDID prefix), but the KWinDisplayDetector path runs in parallel. The D-Bus observer handles both modes correctly by reading `MaxBrightness` from the display's D-Bus property and scaling accordingly.

Forward path works: `SetBrightness iu 5000 0` → D-Bus observer scales 5000/10000 → TV backlight=50. Both the Wayland `requested_brightness` and D-Bus observer fire in SDR mode; the drain-loop deduplicates.

Reverse path: `set_observed_brightness` reaches KWin but the slider never updates. Root cause: KWin's `commit()` is a one-shot — `m_done` prevents `addDevice` from re-firing, so KWin never re-reads the observed brightness after initial assignment.

## Remaining work

1. **Reverse path via D-Bus**: call PowerDevil's `SetBrightness` on `org.kde.ScreenBrightness.Display` directly instead of going through the Wayland `set_observed_brightness` (which is initial-state only). Needs feedback-loop suppression to avoid the D-Bus observer echoing our own writes back. The `last_forward_sent` pattern can be extended: record what the reverse path sends, and have the D-Bus observer skip matching values.

## Future scope

- **Software dimming prevention**: in HDR mode, KWin still applies software brightness alongside our hardware backlight control. The kernel module + upstream KWin changes would be needed to fully eliminate software dimming in HDR.
- **Virtual DRM connector**: DPMS power management for session-independent TV control.
- **PowerDevil BacklightDetector fix**: upstream contribution to make `isInternal()` configurable or EDID-aware for external sysfs backlights.
