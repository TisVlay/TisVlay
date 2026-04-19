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

**Resolved** via a three-piece patch on `kdePackages.powerdevil` (overlay at `~/Documents/nix-config/pkgs/patches/powerdevil-backlight-edid.patch`):

1. **Extend `BacklightDetector` to external displays** — `backlightbrightness.{cpp,h}` and `backlighthelper_linux.{cpp,h}`. The KAuth helper now returns lists of all sysfs backlights (`syspaths`, `brightnesses`, `brightnessmaxes`); the detector constructs one `BacklightBrightness` per entry, each of which reads `$syspath/device/edid` or `$syspath/edid` for its EDID. `isInternal()` flips to `!edidData().has_value()` so EDID-carrying entries present as external.

2. **Make `BacklightDetector` evict `KWinDisplayDetector`** when both report the same EDID — `screenbrightnesscontroller.cpp::onDetectorDisplaysChanged`. When a hardware detector has a non-internal display with EDID, the matching entry from `KWinDisplayDetector` is removed from `newDisplayById` in favor of the hardware one. Combined with excluding `BacklightDetector` from the `shouldUseExternalControl` shunt, this makes the `BacklightDetector` entry the single authoritative `display0` — slider writes hit sysfs directly and `MaxBrightness` reports the real sysfs max (100 for our kmod).

3. **Re-run `BacklightDetector` on backlight hotplug events** — `backlightbrightness.{cpp,h}`. Upstream only runs `detect()` once at `plasma-powerdevil.service` startup; any backlight device that appears after that (our kmod loaded after PowerDevil, late modprobe, USB-C dock display drivers, gpu-screen-recorder virtual outputs) stays invisible until the daemon is manually restarted. `BacklightDetector` now watches the `backlight` udev subsystem for `deviceAdded`/`deviceRemoved` and re-runs `detect()`, emitting `displaysChanged` from every terminal branch so `ScreenBrightnessController` picks up the new set on the re-detect pass. KAuth is async so re-entry is guarded by an `m_isDetecting` / `m_needsRedetect` pair, gated through a `finishDetectionCycle(success)` helper at every success/error path — without the reset-first ordering the re-queue would deadlock against the entry guard.

Patch kept out-of-tree for now; candidate for upstream (the hotplug piece is a legitimate framework bug independent of tisvlay — any dynamic backlight ecosystem hits it).

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

### 7.4 Reverse-path echo bounced sysfs during slider drags

Rapid slider drags in `Kernel` mode made the KDE slider visually yank backward through stale values before settling at the target. The trace, captured with a 25 ms sysfs-sampling loop during a 5 ms/step ramp from 20 to 80:

```
samples  1-14: sysfs 21 → 31 → 46 → ... → 81   (legitimate forward drive)
samples 15-19: sysfs 81 81 81 81 81              (settled briefly)
sample  20:    sysfs 31                           ← TV echo of an earlier forward
sample  22:    sysfs 56
sample  31:    sysfs 66
sample  32+:   sysfs 81                           (finally settled)
```

LG SSAP's subscription delivers ramp-intermediate values as the TV's backlight physically slews. `state_forwarder`'s reverse path fired a `SetBrightness` ioctl on every subscription value that differed from the D-Bus cache, including echoes of forwards we'd just sent — each ioctl went back to sysfs via `backlight_force_update`, PowerDevil's udev observer reported it as external input, and the KDE slider snapped to the stale value.

**Fix landed** ([backlight_driver.rs](../crates/tisvlayd/src/backlight_driver.rs)): a new `PendingForwards` newtype on `BacklightDriver` keeps a per-TV FIFO of values the forward path has sent to the TV. `handle_event` appends before calling `manager.set_backlight`; `state_forwarder`'s SubscriptionUpdate consults `consume_echo(tv_id, level)` which drains everything up through a matching entry and returns `true`. Drain-up-through-match handles TV ramp behavior that echoes only a subset of intermediate values. Genuine TV-remote changes never appear in the queue so `consume_echo` returns `false` and the ioctl still fires — the reverse path from TV to KDE stays intact. Covered by 6 unit tests (`consume_echo_*`, `pending_queue_is_capped_at_max_pending_forwards`).

### 7.5 Forward path lagged the TV ~700 ms behind the slider

With §7.4 fixed (no more visual bounce), a second issue surfaced: on a 16-step rapid drag, sysfs and KDE snap to the target immediately but the daemon's cached `Tv1.Backlight` lags behind by hundreds of milliseconds as it serializes 16 LG SSAP commands at ~40-60 ms each:

```
t+100ms  sysfs=91  KDE=90  daemon=26    ← TV still way behind
t+200ms  sysfs=91  KDE=90  daemon=26
t+400ms  sysfs=91  KDE=90  daemon=61
t+700ms  sysfs=91  KDE=90  daemon=91    ← finally caught up
```

`handle_event` was processing one chardev event per iteration and awaiting the full TV round-trip for each. The session-side forward drain-loop already had a coalescing pattern (`forward_brightness_events` in `brightness/shared.rs`) — extended the same pattern to the daemon.

**Fix landed**: new `coalesce_events(first_evt, &mut event_rx) -> HashMap<String, u8>` helper drains pending events via non-blocking `try_recv` and keeps the latest value per TV. A 16-event burst collapses to one `manager.set_backlight(90)` call — daemon catches up within one SSAP round-trip (~50 ms) instead of ~700 ms. Covered by 5 unit tests (`coalesce_events_*`).

### 7.6 Spurious `ConnectionChanged(false)` on subscription swap killed the kernel device on boot

On first boot with `sync_backlight = "kernel"`, the kernel backlight device would get created then immediately destroyed — end result `/sys/class/backlight/tisvlay-office` missing, no forward/reverse sync, until the user manually re-pushed `SetDesktopMapping`. The journald trace showed:

```
18:18:06  backlight driver task started
18:18:07  SetDesktopMapping pushed          (session, TV not yet connected)
18:18:13  Connected to TV + CC(true)        (on_startup emits true)
18:18:13  Subscribed to backend state updates     (subscription #1)
18:18:14  Subscribed to backend state updates     (subscription #2 — conn replaced)
18:18:14  Backend state subscription ended        (subscription #1's rx drained)
18:18:14  CC(false)                          ← spurious!
18:18:14  D C D C D                          (state_forwarder: true→D+C, true→D+C, false→D)
```

At daemon startup the LG backend establishes two successive WebSocket connections in quick succession (LG's SSAP handshake pattern). Each connection gets its own state subscription task. When the older connection is replaced, its `state_rx` closes out — and the old task's cleanup path unconditionally emitted `ConnectionChanged(false)` regardless of whether the TV was actually disconnected. `state_forwarder` treats any `false` as a reason to destroy the kernel device; the fresh subscription's connection was already seen by the earlier `true` event, so nothing re-created it after the spurious `false`.

**Fix landed** ([tv_manager.rs::run_subscription_task](../crates/tisvlayd/src/tv_manager.rs)): gate the subscription-end emission on actual connection state. Read the TV's `is_connected()` from the shared `tvs` map before emitting — if a replacement connection is up, skip the `false` (log `still_connected=true` for diagnostics). If the TV has been removed from the map entirely (config reload, RemoveTv), early-return with a dedicated log line rather than emitting for a zombie D-Bus path.

The subscription task body was extracted from its inline closure inside `try_subscribe_state` to a free `async fn` (same commit) so this gating is unit-testable without a full `TvManager`. Covered by 3 regression tests using a minimal `FakeConnection`:

- `subscription_end_suppresses_false_when_tv_still_connected` — the bug proper
- `subscription_end_emits_false_when_tv_disconnected` — the reverse-path-intact complement
- `subscription_end_on_removed_tv_emits_nothing` — the let-else early-return path

Verified on hardware: boot ioctl sequence is now `D C D C` ending with C (device present on first boot, no manual `SetDesktopMapping` re-push needed).

## Out of Scope

- GNOME backend. Will motivate the `DesktopBrightnessSync` trait; until then, no trait.
- Auto-detect / probe. Users explicitly pick a variant.
- PowerDevil-patched detection. Picking `Kernel` is a user promise that the patched bin is installed; failure mode is "device exists but PowerDevil ignores it", visible in logs.
- Fallback between variants at runtime (e.g. `Kde` falling through to `Kernel` if the kmod is present). Variants are independent; users rebuild config to switch.
