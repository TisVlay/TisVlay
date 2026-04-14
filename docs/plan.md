# Desktop ↔ TV Sync Plan (Brightness & Volume)

Goal: when the user adjusts KDE Plasma's native brightness or volume control for the display/sink that represents an LG TV, drive the TV's backlight / audio over SSAP. Reuse existing KDE sliders — do not ship new ones.

## Architecture Summary

| Concern      | Primary path (KDE)                                | Portable fallback / notes                                  |
|--------------|----------------------------------------------------|------------------------------------------------------------|
| Brightness   | `kde-external-brightness-v1` Wayland protocol      | Observe `org.kde.ScreenBrightness.BrightnessChanged` (read-only). Kernel `ddcci-backlight` pattern deferred. |
| Volume       | PipeWire virtual sink per TV (`pipewire-rs`)       | Freedesktop-portable; works on any PW system               |
| Identity     | EDID via `libdisplay-info` ↔ SSAP `systemInfo`     | User override in config for unusual setups                 |
| Greeter      | Not supported (powerdevil is user-session)         | Document as known limitation                               |

All work lives in `tisvlay-session` (or a new `tisvlay-desktop-sync` sub-crate if it grows). `tisvlayd` remains the system service owning TV connections; the session service drives it via the existing D-Bus API.

## Phase 1 — Display & sink identity

Prerequisite for both brightness and volume: stable matching between a TV's profile and the desktop object representing it.

- Add `libdisplay-info` bindings (or a small `edid-rs` wrapper) to `tisvlay-core`.
- Extend `UserProfileConfig` (per-TV) with:
  - `display_edid_hash: Option<String>` — auto-populated at pair time, overridable.
  - `audio_sink_name: Option<String>` — PipeWire node name; auto-populated, overridable.
- At session-service startup, enumerate `/sys/class/drm/*/edid`, hash, cross-reference against each configured TV's `systemInfo` (modelName + serialNumber from SSAP). Log matches and any unresolved TVs.
- Expose a `ListDesktopMappings` method on the session D-Bus interface for a future pairing wizard / GUI.

## Phase 2 — Volume sync (PipeWire virtual sink)

PipeWire-only design; no PulseAudio legacy path.

- Add `pipewire-rs` dependency (session service only).
- On session start, for each configured TV:
  - Create a virtual sink named `tisvlay_<tv_id>` with `device.description = <TV friendly name>`, `media.class = Audio/Sink`.
  - Subscribe to `Props` param changes on that node: `channelVolumes`, `mute`, `channelMap`.
  - On change, call the daemon's existing `Tv1.Volume` / `Tv1.Volume.Muted` property setters (no new D-Bus surface needed).
- On TV-side push events (SSAP volume subscription — already wired in `tisvlay-lg`), update the virtual sink's Props so the Plasma slider reflects the TV.
- Tear down the sink on clean session exit; leave it up if the daemon crashes (user can recover by re-logging in).
- Config knob: `volume_sync.enabled: bool` per TV, default `false` until the feature stabilises.
- **Out of scope for Phase 2:** routing audio *through* the virtual sink (loopback to HDMI). Users can pair it with `module-loopback` themselves; we document the recipe.

## Phase 3 — Brightness sync (Wayland external-brightness)

- Add `wayland-client` + the `kde-external-brightness-v1` protocol XML to `tisvlay-session` (codegen via `wayland-scanner`).
- On session start, connect to the user's Wayland display, bind `kde_external_brightness_manager_v1`. If absent (non-KDE or older Plasma), log once and skip.
- For each configured TV with a resolved EDID:
  - Register an `external_brightness_device_v1` with EDID blob, max brightness (e.g. `100` mapped from TV's 0–100 backlight range), and `internal=false`.
  - Handle `requested_brightness` events → call existing `Tv1.Backlight.Backlight` setter.
  - On TV push updates to backlight, call `set_observed_brightness` to keep KDE's slider in sync.
- Config knob: `brightness_sync.enabled: bool` per TV, default `false`.
- **Out of scope for Phase 3:** non-KDE compositors, greeter support, `ddcci-backlight` kernel shim.

## Phase 4 — D-Bus observer fallback (read-only)

For users not running the Wayland client (e.g. remote `tisvlay-cli` driving an unattended setup), offer a best-effort observer-only mode:

- Subscribe to `org.kde.ScreenBrightness.BrightnessChanged` on the session bus.
- Match `display_dbus_name` → TV via `Label` property and the Phase 1 mapping.
- Forward to `Tv1.Backlight`. No write-back to KDE (the Wayland protocol is the only registration path).

Gated behind `brightness_sync.mode = "wayland" | "dbus-observer" | "off"`.

## Phase 5 — Docs, tests, config UX

- New doc `docs/desktop-sync.md` covering the architecture, config examples, KDE version requirements, known limitations (greeter, non-KDE).
- `tisvlay-mock` gains a fake Wayland brightness event source and a fake PipeWire node for harness tests.
- `tisvlay-harness` integration tests: brightness event → SSAP call; PW volume change → SSAP call; SSAP push → slider update.
- `tisvlayctl`/CLI: `tisvlay desktop-sync status` to report what's bound and what's not.

## Explicit Non-Goals (for now)

- Replacing `wayland-idle` with powerdevil signals. Orthogonal; revisit after Phase 3 lands.
- Pre-login / greeter sync. Powerdevil is user-session; SDDM runs separately.
- Userspace-registered `/sys/class/backlight` devices. Not possible without a kernel module.
- GNOME / Sway / Hyprland support. Revisit once `kde-external-brightness-v1` sees cross-compositor adoption, or via a separate DDC/CI backend.
- Shipping a kernel module (`ddcci-backlight`-style) ourselves.

## Open Questions

- Brightness range: SSAP backlight is 0–100 integer. Expose to KDE as `max_brightness=100` (linear), or a higher resolution with interpolation? Pick 100 for Phase 3.
- Virtual-sink volume scale: PipeWire cubic vs linear. Default to cubic to match Plasma's slider curve; verify empirically.
- Debounce: scrolling the KDE slider emits a burst of events. Rate-limit SSAP writes (~20 Hz) with a trailing-edge flush.
- Multi-TV on one HDMI input (matrix switcher): deferred.
