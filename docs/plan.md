# Desktop ↔ TV Sync Plan (Brightness & Volume)

Goal: when the user adjusts KDE Plasma's native brightness or volume control for the display/sink representing a TV, drive the corresponding TV via `tisvlayd`'s existing vendor-agnostic D-Bus surface. Reuse existing desktop sliders — do not ship new ones.

## Vendor & Transport Scoping

This feature lives entirely above the vendor abstraction. It talks to TVs through the existing `org.tisvlay.Tv1.Backlight` / `org.tisvlay.Tv1.Volume` interfaces ([design.md § Feature Interfaces](design.md#feature-interfaces)); no backend-specific calls (SSAP, Tizen, CEC, etc.) appear in this plan or in the new code. Any vendor implementing the relevant `Capabilities` bits (`BACKLIGHT`, `VOLUME`, `MUTE`) gets desktop sync for free.

Scoped by transport, not vendor: desktop sync only makes sense for TVs carrying an AV signal from the host. We gate activation on the TV being reachable **and** having a resolved display/sink association (see Phase 1). This naturally covers HDMI-carrying backends (LG WebOS, hypothetical Samsung Tizen, Sony Bravia, or even a future CEC backend) and naturally excludes backends that wouldn't have a desktop display/sink (e.g. a serial-only projector in another room).

## Architecture Summary

| Concern      | Primary path (KDE)                                     | Portable fallback / notes                                  |
|--------------|--------------------------------------------------------|------------------------------------------------------------|
| Brightness   | `kde-external-brightness-v1` Wayland protocol client   | `org.kde.ScreenBrightness.BrightnessChanged` observer      |
| Volume       | PipeWire virtual sink per TV (`pipewire-rs`)           | Freedesktop-portable; works on any PW system               |
| Identity     | EDID (via `libdisplay-info`) ↔ `Tv1.HardwareId`        | User override in config for unusual setups                 |
| Greeter      | Not supported (desktop-sync services are user-session) | Documented limitation                                      |

Code lives in `tisvlay-session` initially; factor into a `tisvlay-desktop-sync` crate if it grows beyond ~one module per concern. `tisvlayd` stays transport/vendor-neutral and gains only a small identity-exposure addition (below).

## Phase 0 — Vendor-agnostic identity surface

Prerequisite for mapping a KDE display or PipeWire sink to a TV in a vendor-neutral way.

- Add `fn hardware_id(&self) -> Option<HardwareId>` to `TvConnection` (default: `None`). `HardwareId` is a core type with fields `{ manufacturer: Option<String>, model: Option<String>, serial: Option<String> }`. Backends fill what they know (LG via `getSystemInfo`, Samsung via its equivalent, CEC via OSD name + vendor ID, etc.).
- Expose on D-Bus as a read-only `HardwareId` property on `org.tisvlay.Tv1` (struct `(sss)` with empty strings for unknown fields). Already partially paralleled by `DiscoverTvs`'s `hardware_id` tuple field — unify naming.
- Add a new capability bit `HAS_HARDWARE_ID` or simply return `None` when unsupported.
- Mock backend gets a configurable `HardwareId` for tests.

No desktop-sync code yet — this is a standalone daemon-side change that any future feature (GUI pairing wizard, multi-TV disambiguation) will also benefit from.

## Phase 1 — Display & sink identity matching

Still vendor-agnostic; pure session-service logic.

- Add `libdisplay-info` bindings (or an `edid-rs` wrapper) to `tisvlay-session` only.
- Extend `UserProfileConfig` (or a new `[desktop_sync]` table per TV) with:
  - `display_edid_hash: Option<String>` — auto-populated, overridable.
  - `audio_sink_name: Option<String>` — PipeWire node name; auto-populated, overridable.
- At session start, enumerate `/sys/class/drm/*/edid`, hash each, and match against every managed TV's `Tv1.HardwareId` (manufacturer PNPID + model + serial). Log matches and unresolved TVs.
- Session D-Bus: add `ListDesktopMappings() -> a(ssss)` returning `(tv_id, display_name, sink_name, match_source)` for a future pairing wizard / tray UI.

## Phase 2 — Volume sync (PipeWire virtual sink)

PipeWire-only; no PulseAudio-legacy path.

- Add `pipewire-rs` to `tisvlay-session`.
- For each managed TV advertising `Capabilities::VOLUME` via D-Bus:
  - Create a virtual sink `tisvlay_<tv_id>` (`device.description = <Tv1.Name>`, `media.class = Audio/Sink`).
  - Subscribe to its `Props` events: `channelVolumes`, `mute`.
  - On change, drive `Tv1.Volume.Volume` / `Tv1.Volume.Muted` via the existing property setters. No vendor-specific code path.
- On `PropertiesChanged` from `Tv1.Volume`, update the virtual sink's Props so the Plasma slider reflects TV-initiated changes (remote control, etc.).
- Tear down virtual sinks on clean session exit.
- Per-TV config: `desktop_sync.volume = "off" | "virtual_sink"` (default `off` until stable).
- **Out of scope:** loopback routing from the virtual sink to the real HDMI sink. Documented recipe; user adds `module-loopback` if they want it.

## Phase 3 — Brightness sync (Wayland external-brightness)

- Add `wayland-client` and generate bindings for `kde-external-brightness-v1` in `tisvlay-session`.
- On session start, bind `kde_external_brightness_manager_v1`. If absent (non-KDE or pre-6.3), log once and skip brightness sync; volume still works.
- For each managed TV advertising `Capabilities::BACKLIGHT` with a resolved EDID:
  - Register an `external_brightness_device_v1` with the EDID blob, `max_brightness = 100`, `internal = false`.
  - On `requested_brightness`, call `Tv1.Backlight.Backlight` property setter.
  - On `PropertiesChanged` from `Tv1.Backlight`, call `set_observed_brightness` to keep KDE's slider in sync with TV-initiated changes.
- Per-TV config: `desktop_sync.brightness = "off" | "wayland" | "dbus_observer"` (default `off`).

## Phase 4 — D-Bus observer fallback (read-only brightness)

For users who can't or don't want to bind the Wayland protocol client:

- Subscribe to `org.kde.ScreenBrightness.BrightnessChanged`, resolve `display_dbus_name` via the `Label` property and the Phase 1 map, forward to `Tv1.Backlight`.
- Read-only relative to KDE: no back-pressure, no slider update on TV-initiated change. Best-effort.

## Phase 5 — Docs, tests, UX

- New doc `docs/desktop-sync.md`: architecture, vendor-neutrality guarantees, KDE version requirements, greeter gap, enabling/configuring per TV.
- `tisvlay-mock` gains a configurable `HardwareId`, a fake Wayland brightness event source, and a fake PipeWire node so harness tests can exercise both paths without real hardware.
- `tisvlay-harness` integration tests (all vendor-neutral, using mock backend):
  - KDE brightness event → `Tv1.Backlight` D-Bus set → mock-backend state update.
  - PW sink volume change → `Tv1.Volume` set → mock-backend state update.
  - Mock-backend push → `PropertiesChanged` → KDE/PW observed state updated.
- CLI: `tisvlay desktop-sync status` prints the Phase 1 mapping table and active sync modes per TV.

## Explicit Non-Goals

- Any vendor- or protocol-specific branch in desktop-sync code. If a feature needs one, it belongs in the vendor crate behind the existing `TvConnection` trait, not here.
- Replacing `wayland-idle` with KDE power events. Orthogonal; revisit after Phase 3.
- Pre-login / greeter sync. Desktop-sync services are user-session; SDDM is separate.
- Shipping a kernel module (`ddcci-backlight`-style) for non-KDE brightness. Revisit if another compositor adopts a comparable external-brightness protocol.
- GNOME / Sway / Hyprland brightness paths. Volume (PipeWire) already works there.
- Non-HDMI transports. A future CEC backend implementing `Capabilities::BACKLIGHT` + `HardwareId` would Just Work through this feature without any changes here.

## Open Questions

- Brightness range: most backends expose 0–100. Fix `max_brightness = 100` in Phase 3; revisit if a vendor lands with finer granularity.
- PipeWire volume curve: cubic vs linear. Default cubic to match Plasma's slider; verify.
- Debounce policy for slider scroll bursts: trailing-edge flush at ~20 Hz to the D-Bus property setter. Applies uniformly across vendors.
- `HardwareId` serial field: some backends can't read it (CEC). Match falls back to manufacturer + model + user override.
