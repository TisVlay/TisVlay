# Config Enums and Brightness Cleanup Refactor

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

## Implementation Order

1. **Kernel echo fix first.** Isolated change, testable on its own. Rebuild module, reload, confirm no runaway loop when the daemon drives SET_BRIGHTNESS.
2. **Config enum rename.** Introduce the new variants in `tisvlay-core`, update the single existing config (`docs/configs/session.toml`) and any fixture TOMLs, update D-Bus string renderers and `DesktopMapping::from_parts` callers. One commit.
3. **Mutual exclusion in `state_forwarder`.** Gate kernel lifecycle on `sync_backlight == "kernel"`. One commit.
4. **Session module split.** Move existing code into `brightness/{shared,kde_wayland,kde_dbus,kde}.rs`; delete `brightness_sync.rs`; wire coordinator dispatch by enum value. One commit.
5. **Manual verification** for each variant: `None`, `Kernel`, `Kde`, `KdeWayland`, `KdeDbus` — forward and reverse where applicable. Document findings in the commit messages or follow-up doc.

## Out of Scope

- GNOME backend. Will motivate the `DesktopBrightnessSync` trait; until then, no trait.
- Auto-detect / probe. Users explicitly pick a variant.
- PowerDevil-patched detection. Picking `Kernel` is a user promise that the patched bin is installed; failure mode is "device exists but PowerDevil ignores it", visible in logs.
- Fallback between variants at runtime (e.g. `Kde` falling through to `Kernel` if the kmod is present). Variants are independent; users rebuild config to switch.
