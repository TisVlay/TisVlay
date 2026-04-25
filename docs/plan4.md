# Cull Sync, Add DPMS-Driven Power Management

Status: **planning**. The dev branch carries the full Phase 0-3 sync work for posterity (already merged via PR for archive). This plan covers the cull and the simpler power-management replacement that ships in its place.

## 1. Motivation

The Phase 2/3 bidirectional brightness and volume sync features turned out to cost more than they pay back:

- **KDE-specific everywhere**. The brightness path needed a patched `kdePackages.powerdevil` (extend `BacklightDetector` to external displays + EDID-based eviction of `KWinDisplayDetector` + udev hotplug re-detect). Distributing a binary patch against Plasma's release cadence is an ongoing maintenance burden, and the patch is out-of-scope for upstream KDE without much broader buy-in.
- **Wayland brightness isn't truly bidirectional**. KWin's `kde_external_brightness_v1` `set_observed_brightness` handler is one-shot per drag (commit-gated by `m_done`), so a Wayland-only path can't reflect TV-remote changes back onto the slider. Under HDR mode KWin bypasses the protocol entirely and the D-Bus path takes over with different semantics. Two code paths with different correctness models is an attractive nuisance.
- **D-Bus reverse loop is fiddly**. Forward → TV ramp → subscription burst → debounce or echo-match → reverse `SetBrightness` → PowerDevil PropertiesChanged → echo-suppress → done. Every layer needs its own race-free guard, and small mismatches (off-by-one rounding, ramp intermediates not in the queue) produce visible UI bouncing.
- **Volume sync compounds gain levels**. Mirroring TV volume to a PipeWire sink while the user adjusts the desktop mixer effectively chains two gain stages, and the result depends on which side moved last. The mental model is unintuitive and "loud" per-app audio gets clipped or attenuated unpredictably.

The pivot: drop both sync features entirely. Replace the most-asked-for capability they delivered (TV powers off when the desktop sleeps) with a simple session-independent observer that reads DPMS state from sysfs and powers TVs accordingly. No kernel module needed for the common case.

## 2. What Gets Culled

Goal: zero vestigial sync code. Verified by `git diff origin/main -- '**/*.rs' '**/*.toml' '**/*.nix'` having no references to `SyncBacklight`, `SyncVolume`, `brightness/`, `volume_sync`, `last_forward_sent`, `BrightnessMechanism`, `ReverseDebouncer`, `PendingForwards`, `kde_external_brightness`, `tisvlay_backlight` after the cull.

### Files / directories deleted

- `crates/tisvlay-session/src/brightness/` — entire module (mod.rs, kde.rs, kde_dbus.rs, kde_wayland.rs, shared.rs).
- `crates/tisvlay-session/src/volume_sync.rs`.
- `crates/tisvlay-session/src/kde_external_brightness/` — vendored Wayland protocol bindings.
- `module/tisvlay-backlight/` — entire out-of-tree kernel module (Rust + C, kbuild, default.nix).
- `crates/tisvlayd/src/backlight_driver.rs` — chardev integration, `PendingForwards`, `coalesce_events`.
- Patched-PowerDevil overlay reference in NixOS docs.

### Configuration removed

- `SyncBacklight` enum (`tisvlay-core/src/config.rs`) — all 5 variants and serde aliases.
- `SyncVolume` enum.
- `sync_backlight` and `sync_volume` fields on `UserProfileConfig` and `ResolvedUserProfile`.
- `Tv1.Desktop.SyncBacklight` and `Tv1.Desktop.SyncVolume` D-Bus properties (interface trims to identity-only).

### Daemon / session simplification

- `tisvlay-session` survives but loses the `brightness::run` and `volume_sync::run` tasks. The crate's remaining responsibility (logind / screensaver / idle → `Tv1.Power.PowerOff`) gets folded into `tisvlayd` directly, then the `tisvlay-session` binary and its systemd unit get retired. `tisvlay-tray` keeps its existing daemon-only D-Bus surface.
- Daemon subscribes to system-bus `org.freedesktop.login1.Manager.PrepareForSleep` and `org.freedesktop.ScreenSaver.ActiveChanged` directly. No session-bus dependency.

### Tests removed

- All `brightness::*` and `volume_sync::*` unit tests.
- `backlight_driver` chardev tests (`PendingForwards`, `coalesce_events`).
- `kde_external_brightness` doc tests.
- Session-side D-Bus contract tests that asserted on `SyncBacklight` field round-trip — narrow these to identity-only fields.

### Workspace dependencies pruned

If their only consumer was sync code: `wayland-client`, `wayland-protocols`, `wayland-scanner`, `pipewire`, `pipewire-sys`, `libspa`, the C build inputs for the kmod (`pkg-config`, `linuxConsoleTools` if only for kmod testing). Verified by `cargo workspace-unused-pub` + `cargo machete` post-cull.

## 3. What Gets Kept

### Phase 0 — `HardwareId` (kept)

`HardwareId { vendor, model, serial }` on `TvBackend`, populated on first connect by the LG backend from SSAP `getSystemInfo`, exposed on `Tv1.HardwareId` D-Bus property. Costs nothing extra (info already comes back from the TV) and lets users key D-Bus automations on stable per-TV identity. Vendor-agnostic, so future backends (Samsung / Sony / Vizio) drop in cleanly.

### Phase 1 — display matching (kept, repurposed)

`[tv.desktop]` config block keeps `connector` and `edid` fields. `read_edid_for_tv` and the `edidkit` workspace dep stay. The new use: the DPMS observer matches each managed TV to its physical `/sys/class/drm/<connector>` entry by EDID. Same call site, different consumer. The `Tv1.Desktop` D-Bus interface itself shrinks to identity (connector + EDID) and drops the sync_* properties.

### General bug fixes (kept)

- §7.6 spurious `ConnectionChanged(false)` on subscription swap fix (`crates/tisvlayd/src/tv_manager.rs::run_subscription_task` extraction + `is_connected()` gate). Pure correctness, applies regardless of sync.
- Devshell / lockfile / format / clippy churn — re-derived against the culled tree (no need to track individual commits).

### Sync-tied fixes (dropped with their feature)

| Fix | Reason for drop |
|---|---|
| §7.4 `PendingForwards` newtype | Only matters with chardev forward path (kmod gone). |
| §7.5 `coalesce_events` | Same. |
| §7.7 `wait_for_kde_modes` dispatcher gate | Only matters with brightness dispatcher (gone). |
| Plan2 §7.3 `SetTvUserProfile` republishing `DesktopMapping` | The mapping previously carried sync_* values; with sync gone the republish is moot. The general "republish on profile change" pattern stays where it's still relevant (HardwareId, etc.). |
| `BrightnessMechanism::accepts` partition tests | Code under test is gone. |
| `ReverseDebouncer` | Never landed (uncommitted at cull time). |

## 4. DPMS-Driven Power Observer (new)

New module: `crates/tisvlayd/src/dpms_observer.rs`. Owned by `tisvlayd`, no session crate involvement.

### Behavior

1. On daemon start, enumerate `/sys/class/drm/card*-*` directory entries that have `status=connected`.
2. For each managed TV with a recorded EDID in `[tv.desktop]`, find the matching DRM connector by comparing EDID bytes (first 128 base block, base64-encoded — same scheme Phase 1 used).
3. Build a `tv_id → connector_path` map (kept on the `TvManager`).
4. Spawn a polling task that runs at 1 Hz: for each mapped TV, read `/sys/class/drm/<connector>/dpms`.
5. Compare against last-seen state per TV.
6. On `On` → `Off` transition: invoke `Tv1.Power.PowerOff` on that TV (via the daemon's internal API, not D-Bus).
7. On `Off` → `On` transition: invoke `Tv1.Power.PowerOn` (which already handles WoL + reconnect for cold-start cases).
8. Suppress repeats — only fire on transitions, not on each poll.

### Hot-plug handling

The connector-to-TV map gets rebuilt on:

- Daemon startup.
- Config reload (`Manager1.ReloadConfig`).
- udev `change` events on the `drm` subsystem (cheap libudev subscription — fires on hotplug + EDID changes, doesn't fire on DPMS-property changes which is why we still need the poll loop for those).

A TV whose connector vanishes (cable unplugged, KMS reset) is deactivated for DPMS observation; reconnection re-enables it on the next match pass.

### Multi-GPU

Enumeration walks every `card*-*` entry under `/sys/class/drm`. Matching is per-EDID, not per-GPU, so the observer Just Works across GPU vendors / multi-GPU rigs without explicit configuration.

### API shape (testable)

```rust
// Pure data reconciler — testable without filesystem.
pub fn decide_actions(
    tv_states: &[TvDpmsState],
    last_seen: &HashMap<String, DpmsState>,
) -> Vec<DpmsAction> { ... }

// I/O wrapper that reads sysfs and dispatches.
pub async fn run_dpms_observer(
    manager: Arc<TvManager>,
    poll_interval: Duration,
    shutdown: watch::Receiver<bool>,
) { ... }
```

### Echo / debounce concerns

Trivially absent. The DPMS observer only fires on connector-side transitions, never on its own actions. The TV's own state changes don't propagate back to a connector we don't own (see §6 for the limitation), so no feedback loop is possible.

### Future improvements (documented, deferred)

This Phase 1 design is intentionally low-spec to be easily replaceable. Isolation: all sysfs reads happen inside `dpms_observer.rs::poll_dpms_state`; replacing the read mechanism (without changing observer semantics) means swapping that one function. Candidates for follow-up:

- **udev DRM property change events** — newer kernels may grow uevents for DPMS prop changes; if so, replace the poll loop with an event subscription. Today's kernels don't fire these reliably.
- **DRM atomic API as a non-master client** — open `/dev/dri/cardN` without taking master, query connector properties via atomic. Cleaner than sysfs but still polling-only without DRM master, so the win is mostly aesthetic.
- **Compositor-specific event protocols** — KWin's `org_kde_kwin_dpms` and wlroots' `wlr_output_management_v1` are event-driven. GNOME has no exposed DPMS protocol. X11 has `XScreenSaver` extension. These would replace the 1Hz lag with instant response on supported compositors. DE-specific code lives in optional adapters that can plug into the same observer interface.
- **dbus-broker route via logind** — logind already exposes `Inhibit` machinery and `IdleHint`, which maps to "should this seat be sleeping". Cross-checking against logind would give a second signal independent of DRM.

## 5. Detached-TV Power Management (new, fallback)

Some managed TVs are not on a DRM connector (HDMI splitter to a non-PC source, network-only TVs, multi-room setups). The DPMS observer can't help these — there's nothing to observe.

For Phase 1 they're handled by the existing per-TV `idle_timeout_seconds` config in `UserProfileConfig`. The daemon runs an internal timer per TV that resets on any user-initiated activity (`Tv1.Power`, `Tv1.Input`, `Tv1.Volume`, `Tv1.Backlight` D-Bus calls). On expiry the TV powers off. No DRM, no compositor.

Documented but not implemented: see §7 for the Phase 2 virtual-DRM-connector approach, which would let detached TVs participate in compositor-driven DPMS without polling timers.

## 6. Limitations

**TV-initiated power changes don't reach the desktop.** If the user turns the TV off via remote, the kernel's view of the connector's DPMS state stays `On` (the compositor still owns the modeset). We can't push a state change from userspace into a connector we don't own without becoming DRM master, which would conflict with the compositor. Practical impact: when the TV powers itself back on, the desktop's framebuffer is still there and shows up immediately, so the user-visible effect is small. The daemon's `Tv1.Power.IsPowered` property reflects the actual TV state from SSAP subscription, so external tooling can still query the truth.

**1-second lag on DPMS transitions.** Sysfs polls at 1 Hz; the worst-case lag between compositor blank and TV power-off is ~1 second. The TV itself takes 2-3 s to physically transition, so the lag is invisible end-to-end.

**No Wayland-protocol-driven instant response.** Compositor-specific paths are deferred to future work (§4). If someone requests instant response on KWin/wlroots, the adapter slot is reserved.

**Detached TVs rely on idle timer.** If a TV isn't on a DRM connector, only the per-TV `idle_timeout_seconds` triggers power-off. No compositor-coupled signal is available.

## 7. Phase 2 Stub — Virtual DRM Connector (deferred)

For users with TVs not on a PC GPU (HDMI splitter, network-only, multi-room), the long-term desire is for the compositor to manage them as if they were physical displays — i.e. the compositor sees a "virtual" display, and we observe DPMS on it the same way we observe physical connectors today.

Two paths are documented for future research, not implementation:

### Option A — out-of-tree minimal virtual DRM driver

A small kernel module that registers a `drm_device` with one or more `drm_connector` entries, no CRTC / encoder / framebuffer scan-out machinery. Connectors expose `dpms`, `edid`, `status` like real connectors. Compositor manages DPMS state on them. Daemon observes via the same sysfs path as Phase 1. Each managed-but-detached TV gets its own connector. EDID is synthesized from TV identity (or the user pins one).

Cost estimate: significant. DRM is a complex kernel subsystem; even a "minimal" connector driver needs to interact correctly with DRM master semantics, modesetting (compositors will at least probe modes), and the userspace API surface enough for compositors not to choke. Vkms (in-tree virtual KMS) is the reference — too feature-heavy to fork wholesale, but illustrates the integration shape.

Rust-for-Linux DRM bindings are still emerging (the nova-driver work in 6.13+ is the leading edge). A C-only or mostly-C driver is the realistic path today; Rust would be a future migration.

### Option B — userspace virtual-connector bridge

If a future kernel grows a "virtual connector userspace driver" interface (analogous to uinput for input devices), userspace could synthesize a DRM connector without kernel module work. No such interface exists today.

### Option C — extend X11 / Wayland protocols

Lobby for a Wayland protocol that lets non-master clients receive DPMS state notifications across all outputs. Strictly upstream effort, no in-tree work for tisvlay.

The expectation for Phase 2 is that someone hits the detached-TV use case in practice, gauges the cost-benefit, and either builds Option A or accepts the per-TV idle timer fallback.

## 8. Out of Scope (this plan)

- Brightness sync (any direction, any backend).
- Volume sync (any direction, any backend).
- Compositor-specific event protocols for Phase 1 (KWin DPMS, wlroots, X11 — see §4 for future-work).
- TV → desktop awareness when the TV remote changes power state (see §6 limitation).
- udev DRM property-change subscriptions (kernel doesn't fire these reliably; revisit if it changes).
- The virtual DRM connector itself (see §7).

## 9. Migration / Merge Notes

### Branch flow

The dev branch currently carries the full sync history (Phase 0-3 + plan3 refactor) and serves as the public archive. Cull commits stack on top of dev. Each cull commit is small and focused (one feature subdir or config block at a time) so the diff stays reviewable.

When the cull + DPMS observer are complete, dev gets squash-merged into main. The squash flattens the sync work and the cull into a single commit on main; the granular history stays on dev for forensic reference.

### Archive of legacy plans

`docs/plan.md`, `docs/plan2.md`, `docs/plan3.md` move to `docs/archive/` for the duration of the cull (we'll need them as reference while we audit what to remove). Before the squash to main, the entire `docs/archive/` directory is deleted from the merge-bound tree. The original copies survive on the dev branch's git history.

### Cull order (suggested)

1. Move legacy plans to `docs/archive/`.
2. Delete `module/tisvlay-backlight/` and its references in `flake.nix`, `packaging/nix/`.
3. Delete `crates/tisvlayd/src/backlight_driver.rs` and references in `tisvlayd/src/lib.rs`, `state_forwarder.rs`.
4. Delete `crates/tisvlay-session/src/brightness/` and `volume_sync.rs`.
5. Delete `crates/tisvlay-session/src/kde_external_brightness/`.
6. Trim `SyncBacklight`, `SyncVolume` from `tisvlay-core/src/config.rs` and the `Tv1.Desktop` D-Bus interface.
7. Retire the `tisvlay-session` binary and its systemd unit; fold residual logind/screensaver listeners into `tisvlayd`.
8. Run `cargo workspace-unused-pub` and `cargo machete`; prune dead workspace deps.
9. Add `dpms_observer.rs` with reconciler tests + integration glue in `tisvlayd::main`.
10. Add per-TV idle-timer fallback for detached TVs.
11. Re-run `just check` — all tests + clippy + fmt clean.
12. Hardware drill: lock screen → TV powers off within ~3 s; unlock → TV powers on; idle timer for detached TV expires and powers off.
13. Delete `docs/archive/`. Squash-merge dev → main.

### Test plan

- Unit tests for `decide_actions` reconciler — feed synthetic `(tv_states, last_seen)` and assert action lists. Covers transition detection, repeat suppression, hot-plug edge cases.
- Unit test for connector-to-TV matching (EDID-byte equality, ignoring bytes beyond the 128-byte base block).
- Existing NixOS VM `checks.vm` test should pass after fold-down of `tisvlay-session` into `tisvlayd` (the test boots the daemon and exercises the D-Bus surface; nothing brightness-or-volume specific).
- Hardware drill (manual): lock screen, observe TV powers off; unlock, observe TV powers on; configure a detached TV with `idle_timeout_seconds = 60`, leave idle, observe power-off.
- `git diff origin/main -- '**/*.rs' '**/*.toml' '**/*.nix'` produces no hits on `SyncBacklight`, `SyncVolume`, `brightness/`, `volume_sync`, `kde_external_brightness`, `tisvlay_backlight`, `last_forward_sent`, `BrightnessMechanism`, `PendingForwards`, `ReverseDebouncer`, `coalesce_events`.
