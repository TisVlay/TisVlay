# Cull Sync Features

Status: **in progress** — steps 1-6 landed, steps 8/11/12/13 remaining. The dev branch carries the full Phase 0-3 sync work for posterity (already merged via PR for archive). This plan covers the cull only; the event-driven power-management redesign that the original plan4 sketched as a sysfs-DPMS observer is deferred to a follow-up plan (plan5) that ships separately. Today's `tisvlay-session` listener path stays as-is for the cull merge so users keep the lockscreen-fast-blank UX they have now.

## 1. Motivation

The Phase 2/3 bidirectional brightness and volume sync features turned out to cost more than they pay back:

- **KDE-specific everywhere**. The brightness path needed a patched `kdePackages.powerdevil` (extend `BacklightDetector` to external displays + EDID-based eviction of `KWinDisplayDetector` + udev hotplug re-detect). Distributing a binary patch against Plasma's release cadence is an ongoing maintenance burden, and the patch is out-of-scope for upstream KDE without much broader buy-in.
- **Wayland brightness isn't truly bidirectional**. KWin's `kde_external_brightness_v1` `set_observed_brightness` handler is one-shot per drag (commit-gated by `m_done`), so a Wayland-only path can't reflect TV-remote changes back onto the slider. Under HDR mode KWin bypasses the protocol entirely and the D-Bus path takes over with different semantics. Two code paths with different correctness models is an attractive nuisance.
- **D-Bus reverse loop is fiddly**. Forward → TV ramp → subscription burst → debounce or echo-match → reverse `SetBrightness` → PowerDevil PropertiesChanged → echo-suppress → done. Every layer needs its own race-free guard, and small mismatches (off-by-one rounding, ramp intermediates not in the queue) produce visible UI bouncing.
- **Volume sync compounds gain levels**. Mirroring TV volume to a PipeWire sink while the user adjusts the desktop mixer effectively chains two gain stages, and the result depends on which side moved last. The mental model is unintuitive and "loud" per-app audio gets clipped or attenuated unpredictably.

The pivot: drop both sync features entirely. Power-management automation (lockscreen blank, idle, sleep) keeps working through the existing `tisvlay-session` listener path; an event-driven redesign that can do better than today is queued up for plan5 once the cull settles.

## 2. What Gets Culled

Goal: zero vestigial sync code. Verified by `git diff origin/main -- '**/*.rs' '**/*.toml' '**/*.nix'` having no references to `SyncBacklight`, `SyncVolume`, `brightness/`, `volume_sync`, `last_forward_sent`, `BrightnessMechanism`, `ReverseDebouncer`, `PendingForwards`, `kde_external_brightness`, `tisvlay_backlight`, `tisvlay-backlight`, `pipewire`, `kde-external-brightness` after the cull.

### Files / directories deleted

- `crates/tisvlay-session/src/brightness/` — entire module (mod.rs, kde.rs, kde_dbus.rs, kde_wayland.rs, shared.rs).
- `crates/tisvlay-session/src/volume_sync.rs`.
- `crates/tisvlay-session/src/kde_external_brightness.rs` + `protocols/kde-external-brightness-v1.xml` — vendored Wayland protocol bindings.
- `module/tisvlay-backlight/` — entire out-of-tree kernel module (Rust + C, kbuild, default.nix).
- `crates/tisvlayd/src/backlight_driver.rs` — chardev integration, `PendingForwards`, `coalesce_events`.

### Configuration removed

- `SyncBacklight` enum (`tisvlay-core/src/config.rs`) — all 5 variants and serde aliases.
- `SyncVolume` enum.
- `sync_backlight` and `sync_volume` fields on `UserProfileConfig` and `ResolvedUserProfile`.
- `sink` field on `UserTvDesktopConfig` and `DesktopMapping` (PipeWire identifier — only used by volume sync).
- `Tv1.Desktop.SyncBacklight`, `Tv1.Desktop.SyncVolume`, `Tv1.Desktop.Sink` D-Bus properties.

### Daemon / session simplification

- `tisvlay-session` keeps its existing surface (`Session1` D-Bus interface, screensaver / wayland-idle / idle listeners, `event_handler` → `Tv1.Power.SetIsScreenOn`/`PowerOff` / etc.). Only the `brightness::run` and `volume_sync::run` tasks come out, plus their config slots.
- `desktop_probe` stays; its result feeds the (still-existing) `Tv1.Desktop.Edid` / `Connector` properties so tray / GUI / CLI consumers can map TVs to physical connectors.

### Tests removed

- All `brightness::*` and `volume_sync::*` unit tests.
- `backlight_driver` chardev tests (`PendingForwards`, `coalesce_events`).
- `kde_external_brightness` doc tests.
- Session-side D-Bus contract tests that asserted on `SyncBacklight` field round-trip — narrow these to identity-only fields.

### Workspace dependencies pruned

If their only consumer was sync code: `wayland-scanner`, `pipewire-native`, `pipewire-native-spa`, `base64`, the C build inputs for the kmod (`linuxConsoleTools` if only for kmod testing). `wayland-client` and `wayland-protocols` stay because `wayland_idle` (greeter wake detection) still uses them. Verified by `cargo workspace-unused-pub` + `cargo machete` post-cull.

## 3. What Gets Kept

### Phase 0 — `HardwareId` (kept)

`HardwareId { vendor, model, serial }` on `TvBackend`, populated on first connect by the LG backend from SSAP `getSystemInfo`, exposed on `Tv1.HardwareId` D-Bus property. Costs nothing extra (info already comes back from the TV) and lets users key D-Bus automations on stable per-TV identity. Vendor-agnostic, so future backends (Samsung / Sony / Vizio) drop in cleanly.

### Phase 1 — display matching (kept)

`[tv.desktop]` config block keeps `connector` and `edid` fields. `desktop_probe`, `read_edid_for_tv`, and the `edidkit` workspace dep stay. The use case shrinks to "tooling can map managed TVs to physical DRM connectors via EDID" — useful for diagnostics today and for the plan5 event-driven observer when it lands.

### `tisvlay-session` listener path (kept)

Lockscreen blank (`SessionLocked` → `Tv1.Power.SetIsScreenOn(false)`), unlock (`SessionUnlocked` → `Tv1.Power.SetIsScreenOn(true)`), greeter wake via Wayland idle (`GreeterWake` → `SetIsScreenOn(true)`), per-profile idle-timer (`IdleTimeout` → `SetIsScreenOn(false)` / `PowerOff`). All of this stays. The fast-blank/fast-unblank UX is critical — TV stays powered, panel signal toggles in ~100 ms LG round-trip, no cold boot. The plan5 redesign must preserve this latency.

### General bug fixes (kept)

- §7.6 spurious `ConnectionChanged(false)` on subscription swap fix (`crates/tisvlayd/src/tv_manager.rs::run_subscription_task` extraction + `is_connected()` gate). Pure correctness, applies regardless of sync.
- Devshell / lockfile / format / clippy churn — re-derived against the culled tree (no need to track individual commits).

### Sync-tied fixes (dropped with their feature)

| Fix | Reason for drop |
|---|---|
| §7.4 `PendingForwards` newtype | Only matters with chardev forward path (kmod gone). |
| §7.5 `coalesce_events` | Same. |
| §7.7 `wait_for_kde_modes` dispatcher gate | Only matters with brightness dispatcher (gone). |
| Plan2 `SetTvUserProfile` republishing `DesktopMapping` (sync-mode field) | The mapping previously carried sync_* values; with sync gone the republish is moot. The general "republish on profile change" pattern stays where it's still relevant. |
| `BrightnessMechanism::accepts` partition tests | Code under test is gone. |
| `ReverseDebouncer` | Never landed (uncommitted at cull time). |

## 4. Power Management Redesign — Deferred

The original plan4 sketched a sysfs-polled DPMS observer running inside `tisvlayd` to replace the session crate's listener path. Investigation surfaced two issues that argued for deferral:

1. **DPMS doesn't distinguish blank from off** in modern compositors. KDE/X11 collapse to On/Off in practice — both lockscreen-blank and explicit-power-off look identical on `/sys/class/drm/<connector>/dpms`. Mapping DPMS_OFF to `Tv1.Power.PowerOff` would lose today's fast-blank UX (panel signal toggle, ~100 ms) and replace it with a TV cold-boot (5-10 s LG OLED). Mapping to `IsScreenOn` instead works but the user's TV-remote / lockscreen-snap-back path would be capped by the polling rate.
2. **Sysfs polling caps wake-from-lockscreen latency.** At 1 Hz the worst-case lag from "user moves mouse on lockscreen" to "TV unblanks" is ~1 s; today's Wayland idle path is ~100 ms. Adaptive or higher-frequency polling helps but doesn't match an event-driven design.

Rather than ship a polling observer that's known to regress UX and then layer event sources on top, the cleaner move is to leave the session crate alone for the cull merge and design the replacement event-first in plan5. Notes captured below for that effort.

## 5. Future Work — Event-Driven Power Management (plan5 stub)

Pre-thinking that should NOT be lost when plan5 starts:

### Hard requirements

- **Sub-100 ms lockscreen-snap-back.** Mouse jiggle on lockscreen → TV unblanks before the user perceives lag. This is the canonical UX-critical path and rules out 1 Hz polling as a primary signal.
- **Distinguish fast blank from slow power-off.** `Tv1.Power.SetIsScreenOn(false)` keeps the TV powered (fast resume); `Tv1.Power.PowerOff` deep-sleeps the TV (slow resume, requires WoL on wake). The redesign needs to drive the right one based on context. Likely:
  - Lockscreen blank, idle blank → `IsScreenOn(false)`.
  - System suspend (logind PrepareForSleep), explicit user action, long-idle escalation → `PowerOff`.
- **No regression on multi-TV setups.** Each TV gets its own observer instance keyed by EDID-matched DRM connector.
- **Detached TVs (HDMI splitter, network-only) need a fallback.** Per-TV idle timer in the daemon, resets on user-initiated activity.

### Candidate event sources, ranked by latency

| Source | Latency | Coverage | Complexity |
|---|---|---|---|
| **KWin Wayland idle protocol** (`org_kde_kwin_idle`) | ~10 ms | KWin only | Already wired today via `wayland_idle.rs` |
| **wlroots output management** (`wlr_output_management_v1`) | ~10 ms | wlroots-based compositors (sway, hyprland, river) | New adapter |
| **KWin DPMS protocol** (`org_kde_kwin_dpms`) | ~10 ms | KWin only | New adapter |
| **logind `LockedHint` / `IdleHint` PropertiesChanged** | ~50 ms | Any compositor that calls `Session.SetLockedHint` (KDE does, GNOME inconsistent) | System bus, no session-bus dep |
| **DRM atomic API as non-master client** | polling-only without master | Any compositor on Linux | Polling, no event subscription possible |
| **Sysfs `/sys/class/drm/*/dpms` poll** | 1/poll-rate | Any compositor on Linux | Simple but coarse |
| **udev DRM uevents** | event-driven for hotplug, NOT for DPMS prop changes today | Any compositor on Linux | Useless for DPMS-state edges |

### Likely architecture

A small per-DE adapter pattern:

```
tisvlayd
 ├── PowerObserver trait
 │    ├── KdeWaylandObserver (kde_kwin_idle + dpms)
 │    ├── WlrootsObserver (wlr_output_management)
 │    ├── X11Observer (XScreenSaver extension)
 │    └── LogindFallbackObserver (LockedHint + DPMS sysfs poll)
 │         — fallback when no specific adapter matches
 └── Each observer emits ScreenBlank / ScreenWake / Sleep / Wake
    events on a single channel; daemon dispatches to Tv1.Power.*
```

The session crate either gets folded into `tisvlayd` (its session-bus connection becomes the `KdeWaylandObserver`'s home) or one of the adapters runs as a small user-context helper with a tighter D-Bus surface. The right answer depends on whether system-bus tisvlayd can hold a Wayland connection — needs investigation.

### What NOT to do

- Don't ship sysfs-DPMS polling as the primary path. UX regression vs. today.
- Don't try to unify "blank" and "off" semantics. Two distinct LG SSAP code paths with different physics. Pick per-event.
- Don't drop the per-DE adapter pattern in favor of "one universal observer" — each DE exposes session state differently and there's no Linux-wide standard event.

### Out of scope for plan5

- Virtual DRM connector for detached TVs (HDMI splitter scenario) — see §6.
- TV-remote → desktop awareness (we can't push DPMS state into a connector we don't own without DRM master).

## 6. Phase 2 Stub — Virtual DRM Connector (deferred indefinitely)

For users with TVs not on a PC GPU (HDMI splitter, network-only, multi-room), the long-term desire is for the compositor to manage them as if they were physical displays — i.e. the compositor sees a "virtual" display, and the daemon observes DPMS / wake events on it the same way it observes physical connectors.

Three paths documented for future research, not implementation:

### Option A — out-of-tree minimal virtual DRM driver

A small kernel module that registers a `drm_device` with one or more `drm_connector` entries, no CRTC / encoder / framebuffer scan-out machinery. Connectors expose `dpms`, `edid`, `status` like real connectors. Compositor manages DPMS state on them. Daemon observes via the same path as a physical connector. Each managed-but-detached TV gets its own connector. EDID is synthesized from TV identity (or the user pins one).

Cost estimate: significant. DRM is a complex kernel subsystem; even a "minimal" connector driver needs to interact correctly with DRM master semantics, modesetting (compositors will at least probe modes), and the userspace API surface enough for compositors not to choke. Vkms (in-tree virtual KMS) is the reference — too feature-heavy to fork wholesale, but illustrates the integration shape.

Rust-for-Linux DRM bindings are still emerging (the nova-driver work in 6.13+ is the leading edge). A C-only or mostly-C driver is the realistic path today; Rust would be a future migration.

### Option B — userspace virtual-connector bridge

If a future kernel grows a "virtual connector userspace driver" interface (analogous to uinput for input devices), userspace could synthesize a DRM connector without kernel module work. No such interface exists today.

### Option C — extend X11 / Wayland protocols

Lobby for a Wayland protocol that lets non-master clients receive output state notifications across all connectors. Strictly upstream effort, no in-tree work for tisvlay.

The expectation is that someone hits the detached-TV use case in practice, gauges the cost-benefit, and either builds Option A or accepts a per-TV idle-timer fallback in plan5.

## 7. Out of Scope (this plan)

- Brightness sync (any direction, any backend).
- Volume sync (any direction, any backend).
- Power-management redesign (deferred to plan5 — see §4 / §5).
- The virtual DRM connector itself (see §6).

## 8. Migration / Merge Notes

### Branch flow

The dev branch currently carries the full sync history (Phase 0-3 + plan3 refactor) and serves as the public archive. Cull commits stack on top of dev. Each cull commit is small and focused (one feature subdir or config block at a time) so the diff stays reviewable.

When the cull is complete, dev gets squash-merged into main. The squash flattens the sync work and the cull into a single commit on main; the granular history stays on dev for forensic reference.

### Archive of legacy plans

`docs/plan.md`, `docs/plan2.md`, `docs/plan3.md` move to `docs/archive/` for the duration of the cull (we'll need them as reference while we audit what to remove). Before the squash to main, the entire `docs/archive/` directory is deleted from the merge-bound tree. The original copies survive on the dev branch's git history.

### Cull order

1. ~~Move legacy plans to `docs/archive/`.~~ ✓
2. ~~Delete `module/tisvlay-backlight/` and its references in `flake.nix`, `packaging/nix/`.~~ ✓
3. ~~Delete `crates/tisvlayd/src/backlight_driver.rs` and references in `tisvlayd/src/lib.rs`, `state_forwarder.rs`.~~ ✓
4. ~~Delete `crates/tisvlay-session/src/brightness/` and `volume_sync.rs`.~~ ✓
5. ~~Delete `crates/tisvlay-session/src/kde_external_brightness.rs` + protocol XML.~~ ✓
6. ~~Trim `SyncBacklight`, `SyncVolume` from `tisvlay-core/src/config.rs` and the `Tv1.Desktop` D-Bus interface.~~ ✓
6b. ~~Drop the `sink` field on `UserTvDesktopConfig` / `DesktopMapping` and `Tv1.Desktop.Sink`.~~ ✓
7. Run `cargo workspace-unused-pub` and `cargo machete`; prune dead workspace deps.
8. Re-run `just check` — all tests + clippy + fmt clean.
9. Hardware drill: lockscreen blank still triggers `Tv1.Power.SetIsScreenOn(false)`; unlock + greeter wake still triggers `SetIsScreenOn(true)` within ~100 ms; idle timeout still fires; no orphaned brightness or volume references in the journal.
10. Delete `docs/archive/`. Squash-merge dev → main.

### Test plan

- Existing NixOS VM `checks.vm` test should pass unchanged (the test boots the daemon and exercises the D-Bus surface; nothing brightness-or-volume specific).
- Existing session-bus contract tests in `crates/tisvlay-session/tests/session_dbus_contract.rs` and `session_events.rs` validate the listener path is unbroken.
- Hardware drill (manual): lock screen, observe TV blanks via `IsScreenOn=false`; mouse-jiggle the greeter, observe TV unblanks within ~100 ms; explicit idle timeout fires.
- `git diff origin/main -- '**/*.rs' '**/*.toml' '**/*.nix'` produces no hits on `SyncBacklight`, `SyncVolume`, `brightness/`, `volume_sync`, `kde_external_brightness`, `tisvlay_backlight`, `last_forward_sent`, `BrightnessMechanism`, `PendingForwards`, `ReverseDebouncer`, `coalesce_events`, `pipewire`, plus a final sweep on `Sink`/`sink_changed`/`sync_backlight_changed`.
