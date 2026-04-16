# Virtual Backlight Kernel Module Plan

Sub-plan for Phase 3 of [plan.md](plan.md). Creates `/sys/class/backlight/tisvlay-*` devices so KDE (and any DE) sees managed TVs as hardware-backlight displays — no software dimming, full bidirectional sync with `Tv1.Backlight`.

## Why a kernel module?

KDE Plasma's PowerDevil selects a brightness controller per display in priority order:

1. **KWinDisplayDetector** — software brightness via compositor (MaxBrightness=10000)
2. **BacklightDetector** — hardware backlight via `/sys/class/backlight/`
3. **DDCutilDetector** — hardware DDC/CI

When `BacklightDetector` or `DDCutilDetector` claims a display, KWin stops advertising `BrightnessControl` for it — no software dimming is applied. The `kde-external-brightness-v1` Wayland protocol can then intercept those displays for external control.

Our TV has neither:
- No `/sys/class/backlight/` entry (not a laptop panel)
- No DDC/CI (`ddcutil` confirms "I2C slave address x37 is unresponsive" on the LG OLED42C3PUA)

So KWin claims it with software brightness. The Wayland protocol explicitly skips KWin-claimed displays. A virtual backlight device breaks this deadlock: `BacklightDetector` claims the TV, software brightness disappears, and KDE drives the OLED backlight directly.

This is the same pattern used by `ddcci-backlight` (creates `/sys/class/backlight/ddcci*` for DDC/CI monitors) and is how laptop panels have always worked (ACPI/sysfs backlight → no software dimming).

## Prior art

- **`ddcci-backlight`** — out-of-tree kernel module, creates sysfs backlight devices for DDC/CI monitors. Well-maintained, packaged in NixOS (`linuxPackages.ddcci-driver`). Our module follows the same registration pattern but with a userspace control channel instead of I2C.
- **Laptop backlight drivers** (`acpi_video`, `apple_bl`, `nvidia_bl`) — each registers a `backlight_device` with the kernel's backlight class; KDE discovers them via sysfs.

## Architecture

```
┌─────────────────────┐     sysfs write      ┌──────────────────────┐
│  KDE PowerDevil     │ ──────────────────>   │  /sys/class/backlight│
│  (BacklightDetector)│ <──────────────────   │  /tisvlay-<tv_id>/   │
│                     │     sysfs read        │  brightness          │
└─────────────────────┘                       └──────────┬───────────┘
                                                         │ kernel
                                              ┌──────────┴───────────┐
                                              │  tisvlay_backlight   │
                                              │  kernel module       │
                                              │                      │
                                              │  /dev/tisvlay-bl     │
                                              │  (misc chardev)      │
                                              └──────────┬───────────┘
                                                         │ uevent / ioctl / chardev
                                              ┌──────────┴───────────┐
                                              │  tisvlayd            │
                                              │  (system daemon)     │
                                              │                      │
                                              │  Tv1.Backlight       │
                                              └──────────────────────┘
```

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

### EDID association

KDE's `BacklightDetector` matches backlight devices to KScreen outputs by sysfs topology (looking for the DRM connector in the device tree). For out-of-tree platform devices this doesn't work automatically. Two options:

1. **Name convention**: create the backlight device with a name that KDE can match heuristically. `ddcci-backlight` uses `ddcci<bus>` and KDE falls back to EDID matching.
2. **Explicit sysfs link**: the module creates a `device` symlink pointing at the DRM connector's sysfs path (e.g., `/sys/class/drm/card1-HDMI-A-1`). KDE follows this link for output matching.

Option 2 is more robust. The daemon passes the DRM connector path at device creation time (already known from the Phase 1 desktop probe).

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

The existing `sync_backlight` enum gains a new variant:

```rust
pub enum SyncBacklight {
    None,
    Wayland,     // dead end — kept for config compat, logs a warning
    Backlight,   // virtual backlight kernel module (new)
}
```

Or rename `Wayland` → `Backlight` since the Wayland path is defunct. The config value in user profiles would be `sync_backlight = "backlight"`.

## Scope and ordering

1. **Kernel module** (`crates/tisvlay-backlight/` or `module/`): C code, Makefile, NixOS package derivation. ~200 lines of C.
2. **Daemon chardev integration** (`tisvlayd`): Rust, new module `backlight_driver.rs`. ~300 lines.
3. **Session service**: no new code — the decision to enable sync is already pushed via `DesktopMapping`. The daemon handles everything.
4. **NixOS module**: `extraModulePackages` + udev rule. ~10 lines of Nix.

## Open questions

- **Connector path at create time**: the daemon needs the DRM connector sysfs path to create the symlink. The session service already knows this (Phase 1 desktop probe → `[tv.desktop].connector`). The daemon receives it via `DesktopMapping`. Need to verify the sysfs path is derivable from the connector name (e.g., `HDMI-A-1` → `/sys/class/drm/card1-HDMI-A-1`).
- **Multi-GPU**: if the user has multiple GPUs, the `card` number in the DRM path varies. The desktop probe already handles this (it walks `/sys/class/drm/`), so the full sysfs path should be passed rather than derived from a card number assumption.
- **Module signing**: NixOS doesn't enforce module signing by default, but Secure Boot setups do. Flag for later if users report issues.
- **Backlight device naming**: `tisvlay-<tv_id>` may need sanitization (tv_id can contain arbitrary characters from user config). Restrict to `[a-z0-9_-]` in the ioctl handler.
