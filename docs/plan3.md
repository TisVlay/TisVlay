# Config Enums and Brightness Cleanup Refactor

Status: **implemented and verified end-to-end** for `Kde` and `Kernel` modes (see §6). `KdeWayland` / `KdeDbus` strict variants still pending hardware testing.

Refactors the brightness synchronization architecture to fix oscillation loops and cleanly isolate the competing approaches (Kernel vs. Wayland vs. D-Bus). The project is pre-release and greenfield, so there are no backwards-compatibility constraints — config values get renamed cleanly, no serde aliases.

## Goals

- One config value selects one brightness backend. Never two at once.
- Oscillation loop between daemon (kernel path) and session (Wayland + D-Bus) eliminated by mutual exclusion at the configuration boundary.
- Kernel-side echo loop (ioctl SET → update_status → push_event → daemon forwards → TV echoes → …) killed at the kernel module.
- Session-side module clutter replaced by a clean module split (no premature trait abstraction).

## 1. Unified Configuration Enum (`SyncBacklight`)

Flat enum so TOML stays a single string. `Kde` is the friendly default that handles both HDR and SDR; `KdeWayland` and `KdeDbus` are strict overrides for advanced users and debugging.

### `crates/tisvlay-core/src/config.rs`

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncBacklight {
    /// Sync disabled.
    #[default]
    None,

    /// Native sysfs via the `tisvlay_bl` kernel module. Bidirectional.
    /// Requires the kmod loaded and a patched PowerDevil that matches
    /// `/sys/class/backlight/tisvlay-*` by EDID.
    Kernel,

    /// Default KDE session backend. KWin's Wayland `requested_brightness`
    /// drives the forward path under SDR; PowerDevil's D-Bus observer
    /// takes over transparently under HDR. Reverse path calls PowerDevil's
    /// `SetBrightness`. Bidirectional.
    Kde,

    /// Strict: KWin Wayland protocol only. SDR only. Forward-only — KDE's
    /// slider does NOT follow TV-remote changes because KWin's
    /// `set_observed_brightness` handler is one-shot.
    KdeWayland,

    /// Strict: PowerDevil D-Bus only. Works in both SDR and HDR.
    /// Bidirectional.
    KdeDbus,
}
```

Rendering on the D-Bus `Tv1.Desktop.SyncBacklight` property uses the same snake_case strings (`"none"`, `"kernel"`, `"kde"`, `"kde_wayland"`, `"kde_dbus"`) so shell tooling stays consistent with the config file.

## 2. Mutual Exclusion at the Config Boundary

### `crates/tisvlayd/src/state_forwarder.rs`

`sync_backlight_device` currently creates the kernel device whenever `mapping.sync_backlight != "none"`. Change it to activate **only** when the resolved mapping is `"kernel"`. All other values (`"kde"`, `"kde_wayland"`, `"kde_dbus"`) short-circuit to `Destroy` so the kernel device disappears even if a stale one was present.

### `crates/tisvlay-session/src/brightness/` (new; see §3)

The coordinator reads the resolved `sync_backlight` string per TV and dispatches to exactly one backend. `None` and `Kernel` are early no-ops on the session side — no Wayland device registration, no D-Bus observer, no reverse-path subscription.

End result: under `sync_backlight = "kernel"`, the session crate is inert for that TV and only the daemon talks to sysfs. Under `sync_backlight = "kde"`, the daemon's kernel driver is inert for that TV and only the session crate drives brightness.

## 3. Session Module Split

Break the `brightness_sync.rs` monolith into a module directory. No trait — three structs living in three files, each exposing a narrow `run(...)` entry point. A trait can be introduced later when a non-KDE backend (GNOME) lands; at that point the refactor is mechanical because the shapes already line up.

```
crates/tisvlay-session/src/brightness/
  mod.rs        coordinator: reads config, picks backend, owns Shared state,
                defines BrightnessEvent and DeviceOp
  shared.rs     DE-agnostic types: Shared, last_forward_sent, tv_display_info
  kde_wayland.rs
                WaylandState, event queue loop, requested_brightness handler.
                Forward-only. Used by KdeWayland and composed into Kde.
  kde_dbus.rs   PowerDevil PropertiesChanged observer (forward) +
                SetBrightness caller (reverse). Used by KdeDbus and
                composed into Kde.
  kde.rs        Orchestrator for the default Kde backend. Spawns both
                kde_wayland and kde_dbus and funnels their events through
                last_forward_sent dedupe.
```

`mod.rs` exposes a single `pub async fn run(config, state, shutdown) -> Result<()>` that replaces the current `brightness_sync::run`. Its dispatcher:

```rust
match sync_backlight {
    SyncBacklight::None | SyncBacklight::Kernel => return Ok(()),
    SyncBacklight::KdeWayland => kde_wayland::run(...).await,
    SyncBacklight::KdeDbus    => kde_dbus::run(...).await,
    SyncBacklight::Kde        => kde::run(...).await,
}
```

`kde::run` composes the other two. Both push into a single `mpsc::UnboundedSender<BrightnessEvent>`. The drain loop applies `last_forward_sent` dedupe before calling `daemon.set_backlight(...)`, so parallel events from Wayland and D-Bus for the same user action collapse to one TV write.

## 4. Kernel Echo Fix

### `module/tisvlay-backlight/helpers.c`

In `tisvlay_bl_set`, replace the `backlight_update_status(bd)` call with `backlight_force_update(bd, BACKLIGHT_UPDATE_HOTKEY)`. The first invokes `ops->update_status`, which in our module is `tisvlay_bl_update_status` → `push_event` — every daemon-initiated sysfs write echoes back as a forward event. The second updates `props.brightness` (via `get_brightness`) and emits `sysfs_notify` / uevent so PowerDevil still observes the change, without firing our ops callback.

After the fix: one user action = one TV write, zero ioctl → push_event echo round trips.

## 5. Implementation Order

1. **Kernel echo fix first** ✅ — `helpers.c:tisvlay_bl_set` now uses `backlight_force_update(BACKLIGHT_UPDATE_HOTKEY)`.
2. **Config enum rename** ✅ — `SyncBacklight::Kernel|Kde|KdeWayland|KdeDbus`; call sites, fixture tests, and `docs/configs/session.toml` migrated.
3. **Mutual exclusion in `state_forwarder`** ✅ — kernel lifecycle gated on `sync_backlight == "kernel"`; cascaded from the enum rename.
4. **Session module split** ✅ — `brightness/{shared,kde_wayland,kde_dbus,kde,mod}.rs`; `brightness_sync.rs` deleted; coordinator dispatches by enum value.
5. **Manual verification** — partial, see §6.

## 6. Verification Findings

End-to-end on hardware (LG OLED42C3PUA, HDR, KDE Plasma 6.6.4 with patched PowerDevil):

- **`Kde`**: ✅ Forward (KDE slider → TV) and reverse (TV remote → KDE slider) both work. Forward flows through `kde_external_brightness_v1` (KWin's `requested_brightness`); reverse flows through PowerDevil D-Bus `SetBrightness` from our observer.
- **`Kernel`**: ✅ Forward and reverse both work with the extended PowerDevil patch (§7.1). KDE slider writes sysfs via the patched `BacklightDetector`, kernel module forwards to the daemon, daemon pushes to the TV. TV-remote changes fire `sysfs_notify`/uevent through `backlight_force_update`, which PowerDevil's `BacklightBrightness` observes and reports back to the KDE slider.
- **`KdeWayland`** / **`KdeDbus`**: ⚠️ Not yet exercised end-to-end. Code paths are covered by the same module-split refactor and share primitives with `Kde`; verification pending.
- **`None`**: ✅ Session brightness dispatcher exits at `info!("no KDE-flavor TVs, session brightness sync idle")`; daemon's kernel driver stays inert (no `CREATE` ioctl).

## 7. Known Limitations

### 7.1 `Kernel` mode requires an extended PowerDevil patch

Out-of-the-box KDE Plasma, PowerDevil's `ScreenBrightnessController` picks detectors with the priority `KWinDisplayDetector > BacklightDetector > DDCutilDetector`. `KWinDisplayDetector` runs first and owns `/org/kde/ScreenBrightness/display0` with KDE's 0-10000 internal scale — slider writes update KWin's compositor-side brightness (for HDR tone-mapping) without ever reaching sysfs. The kernel module is installed and observing but effectively orphaned from the user-facing slider.

**Resolved** via a two-piece patch on `kdePackages.powerdevil` (overlay at `~/Documents/nix-config/pkgs/patches/powerdevil-backlight-edid.patch`):

1. **Extend `BacklightDetector` to external displays** — `backlightbrightness.{cpp,h}` and `backlighthelper_linux.{cpp,h}`. The KAuth helper now returns lists of all sysfs backlights (`syspaths`, `brightnesses`, `brightnessmaxes`); the detector constructs one `BacklightBrightness` per entry, each of which reads `$syspath/device/edid` or `$syspath/edid` for its EDID. `isInternal()` flips to `!edidData().has_value()` so EDID-carrying entries present as external.

2. **Make `BacklightDetector` evict `KWinDisplayDetector`** when both report the same EDID — `screenbrightnesscontroller.cpp::onDetectorDisplaysChanged`. When a hardware detector has a non-internal display with EDID, the matching entry from `KWinDisplayDetector` is removed from `newDisplayById` in favor of the hardware one. Combined with excluding `BacklightDetector` from the `shouldUseExternalControl` shunt, this makes the `BacklightDetector` entry the single authoritative `display0` — slider writes hit sysfs directly and `MaxBrightness` reports the real sysfs max (100 for our kmod).

Patch kept out-of-tree for now; candidate for upstream once the EDID attribute convention stabilizes and the kernel module (or a generalization of it) is production-ready.

The tisvlay kernel module exports the EDID blob via `DEVICE_ATTR_RO(edid)` on the backlight device, which matches what the patched `BacklightBrightness::edidData()` expects. Matching bytes between `/sys/class/backlight/tisvlay-<tv>/edid` and `KScreen::Output::edid->rawData()` is what keys the eviction.

**Cosmetic follow-ups** (not blocking):

- PowerDevil renders our display's `Label` as `"Built-in Screen"` (its fallback when `BacklightBrightness::label()` has nothing vendor-provided). Could be improved by exposing a `display_name` attribute from the kernel module or by teaching `BacklightBrightness::label()` to parse the monitor name from EDID.
- Slight off-by-one rounding: setting `display0` to 40 lands as sysfs `41`. Within PowerDevil's `knownSafeMinBrightness` / ratio semantics. Not worth chasing.

### 7.2 Session dispatcher startup race

`SessionState::load` returns an empty `profiles` map when the daemon is offline at boot. The initial dispatcher snapshotted that empty state once and exited to the "no KDE-flavor TVs" branch forever, leaving brightness sync disabled until the user restarted the session.

**Fix landed** (`brightness/mod.rs::wait_for_modes`): the dispatcher polls `state.profiles` once per second until it's non-empty or shutdown fires, then dispatches. Covered by `run_does_not_exit_while_state_is_empty` regression test.

### 7.3 `SetTvUserProfile` didn't republish `DesktopMapping`

Flipping a TV's user profile via the tray (or D-Bus) updated `state.profiles` and the user config on disk but never called `Manager1.SetDesktopMapping` on the daemon. The daemon's cached `Tv1.Desktop.SyncBacklight` stayed stale, so `state_forwarder` couldn't gate the kernel backlight device lifecycle on the new mode.

**Fix landed** (`session_dbus::set_tv_user_profile`): rebuild `DesktopMapping` inside the write lock, drop the lock, push to the daemon. Covered by `set_tv_user_profile_republishes_desktop_mapping` regression test. `TestSession::with_user_config_path` was added alongside to isolate tests from `~/.config/tisvlay/config.toml`.

## Out of Scope

- GNOME backend. Will motivate the `DesktopBrightnessSync` trait; until then, no trait.
- Auto-detect / probe. Users explicitly pick a variant.
- PowerDevil-patched detection. Picking `Kernel` is a user promise that the patched bin is installed; failure mode is "device exists but PowerDevil ignores it", visible in logs.
- Fallback between variants at runtime (e.g. `Kde` falling through to `Kernel` if the kmod is present). Variants are independent; users rebuild config to switch.
