# Desktop ↔ TV Sync Plan (Brightness & Volume)

Goal: when the user adjusts KDE Plasma's native brightness or volume control for the display/sink representing a TV, drive the corresponding TV via `tisvlayd`'s existing vendor-agnostic D-Bus surface. Reuse existing desktop sliders — do not ship new ones.

## Grounding in the Current Codebase

Everything below builds on code that already exists. Relevant anchors:

- **Capabilities** — bitflags in [capabilities.rs:10-91](../crates/tisvlay-core/src/capabilities.rs): `POWER`, `SCREEN`, `VOLUME`, `MUTE`, `INPUT`, `BACKLIGHT`, `WOL`, `DISCOVERY`. No identity bit today.
- **`TvConnection` trait** — [traits.rs](../crates/tisvlay-core/src/traits.rs) with `set_volume`/`get_volume`/`set_mute`/`is_muted` (lines 187-204), `set_backlight`/`get_backlight` (lines 238-251), `subscribe_state` (line 265). Default impls return `Error::not_supported` so backends override only what they support. **No `hardware_id()` today.**
- **D-Bus cached properties** — [dbus/tv.rs:280-416](../crates/tisvlayd/src/dbus/tv.rs): `TvVolumeService` caches `volume: u8` + `muted: bool`, `TvBacklightService` caches `backlight: u8`. Property setters call through the manager and update cache on success; zbus emits `PropertiesChanged` automatically.
- **State forwarder** — [state_forwarder.rs:157-211](../crates/tisvlayd/src/state_forwarder.rs): backend push → `iface_ref.get_mut()` → mutate cache → call `iface.volume_changed(emitter).await`. Observers subscribe via standard `org.freedesktop.DBus.Properties.PropertiesChanged`.
- **Daemon client / proxies** — [dbus_client.rs](../crates/tisvlay-core/src/dbus_client.rs): `TisvlayTvVolumeProxy`, `TisvlayTvBacklightProxy`, cached in `DaemonClient.tv_proxies` (Arc<RwLock<HashMap>>). Already consumed by the session service.
- **Session service scaffold** — [tisvlay-session/src/main.rs:95-194](../crates/tisvlay-session/src/main.rs): retry-loop + `tokio::spawn` event-handler pattern, channel-based coordination. `Cargo.toml` already pulls in `wayland-client` + `wayland-protocols` (used by `wayland_idle.rs`).
- **Vendor config flatten** — [config.rs:560-628](../crates/tisvlay-core/src/config.rs): `TvConfig { #[serde(flatten)] vendor_config: HashMap<String, toml::Value> }`. User-owned per-TV state already lives in `ResolvedUserProfile`.
- **`hardware_id` today** — a `String` field on `DiscoveredTv` (dbus_client.rs) populated as `""` by [tisvlay-lg/src/discovery.rs:110-117](../crates/tisvlay-lg/src/discovery.rs). Unused downstream. Consolidating into a first-class identity surface replaces dead code; it doesn't duplicate anything.
- **Mock backend** — [tisvlay-mock/src/state.rs](../crates/tisvlay-mock/src/state.rs) + [handle.rs](../crates/tisvlay-mock/src/handle.rs): no backlight/volume state today; `publish_tv` / `simulate_user_accept` are the pattern to mirror.
- **PipeWire** — not in any `Cargo.toml`. Needs adding (to `tisvlay-session` only).

## Vendor & Transport Scoping

This feature lives entirely above the vendor abstraction. It drives TVs through `org.tisvlay.Tv1.Backlight` / `org.tisvlay.Tv1.Volume` ([design.md § Feature Interfaces](design.md#feature-interfaces)); no backend-specific calls (SSAP, Tizen, CEC, etc.) appear here or in the new code. Any vendor advertising `Capabilities::{BACKLIGHT, VOLUME, MUTE}` gets desktop sync for free.

Scoped by transport, not vendor: sync only makes sense for TVs carrying an AV signal from the host. Activation is gated on `Capabilities` + a resolved display/sink association (Phase 1). This covers HDMI-carrying backends (LG WebOS, a hypothetical Samsung Tizen / Sony Bravia / future CEC backend) and naturally excludes e.g. a serial-only projector.

## Architecture Summary

| Concern      | Primary path (KDE)                                     | Portable fallback / notes                                  |
|--------------|--------------------------------------------------------|------------------------------------------------------------|
| Brightness   | `kde-external-brightness-v1` Wayland protocol client   | `org.kde.ScreenBrightness.BrightnessChanged` observer      |
| Volume       | PipeWire virtual sink per TV (`pipewire-native`)       | Freedesktop-portable; works on any PW system               |
| Identity     | EDID (via `edidkit`) ↔ `Tv1.HardwareId`        | User override in user config                               |
| Greeter      | Not supported (desktop-sync runs in the user session)  | Documented limitation                                      |

New code lives in `tisvlay-session` (add a `desktop_sync` module). Split into a `tisvlay-desktop-sync` sub-crate only if the module set grows beyond ~3 files. `tisvlayd` stays transport/vendor-neutral; its only additions are Phase 0.

## Phase 0 — Vendor-agnostic identity surface (`tisvlayd` + `tisvlay-core`)

Prerequisite for mapping KDE displays / PipeWire sinks to TVs without caring which vendor a given TV is.

- Add a `HardwareId` struct to `tisvlay-core` with fields `{ manufacturer: Option<String>, model: Option<String>, serial: Option<String> }`. Manufacturer uses EDID PNPID convention (e.g. `GSM` for LG, `SAM` for Samsung) so cross-referencing EDID is mechanical.
- Add `fn hardware_id(&self) -> Option<HardwareId> { None }` to `TvConnection` ([traits.rs](../crates/tisvlay-core/src/traits.rs)) as a default-returning-`None` method — matches the existing optional-feature pattern without needing a new capability bit. Skip `HAS_HARDWARE_ID`.
- Populate the LG implementation from the existing `getSystemInfo` / `getSystemSettings` response in [tisvlay-lg/src/client.rs](../crates/tisvlay-lg/src/client.rs) (already cached; no new SSAP call required).
- Replace the unused `String hardware_id` on `DiscoveredTv` with `Option<HardwareId>`. Update [tisvlay-lg/src/discovery.rs:110-117](../crates/tisvlay-lg/src/discovery.rs) to fill it from the SSDP `X-*` headers + cached system info.
- Expose on D-Bus as a read-only `HardwareId` property on `org.tisvlay.Tv1` (struct `(sss)`, empty strings = unknown). Add a `TisvlayTvHardwareProxy` in [dbus_client.rs](../crates/tisvlay-core/src/dbus_client.rs) mirroring the existing per-interface proxies.
- Mock backend: add `hardware_id: Option<HardwareId>` to `MockState` with a setter on `MockHandle` for tests. Mirror the `publish_tv` pattern in [handle.rs:72](../crates/tisvlay-mock/src/handle.rs).

Standalone value: a future pairing wizard / GUI also wants this, independently of desktop sync.

## Phase 1 — Display & sink identity matching (`tisvlay-session`)

- Add `edidkit` to `tisvlay-session`.
- Add a `desktop` section to `UserConfig` (not system config — this is a per-user preference, parallel to `ResolvedUserProfile`):
  ```toml
  [[tv]]
  id = "living-room"
  [tv.desktop]
  edid = "abc123…"           # identity — EDID hash of the matching monitor; auto, overridable
  sink = "alsa_output.…"     # identity — PipeWire sink node name; auto, overridable
  sync.backlight = "none"    # behavior — none | wayland   (dbus_observer added in Phase 4)
  sync.volume    = "none"    # behavior — none | virtual_sink
  ```
  Field naming matches existing project conventions: one-word section name (`desktop` alongside `profile`/`vendor`), `snake_case` throughout, enum variants mirror `SessionAction::None`'s `"none"` rather than `"off"`, and `sync.backlight` uses the `Tv1.Backlight` interface name rather than the KDE-side term "brightness". Identity fields (`edid`, `sink`) sit at the section root so future non-sync consumers (tray readout, pairing wizard, input-switch automation) can reuse them without going through the sync subsystem.
- At session start, enumerate `/sys/class/drm/*/edid`, parse via `edidkit`, and match against every managed TV's `Tv1.HardwareId` (manufacturer PNPID + model + serial with fallback to manufacturer+model when serial is unavailable, e.g. CEC).
- Session D-Bus (`org.tisvlay.Session1`, [session_dbus.rs](../crates/tisvlay-session/src/session_dbus.rs)): add `ListDesktopMappings() -> a(ssss)` returning `(tv_id, display_name, sink_name, match_source)` for a future tray/GUI surface.

## Phase 2 — Volume sync (PipeWire virtual sink)

- Use [`pipewire-native`](https://crates.io/crates/pipewire-native) (pure-Rust, no libpipewire FFI, KDE-aligned — checkout at `/home/ruffsl/ws/kde/libs/pipewire-native-rs`). Our needs — virtual sink creation + `Props` subscribe/set, no audio streaming — fall within what the crate supports today. Greenfield + Rust-only: no libpipewire FFI dependency, no adapter layer, just pin a commit and move with upstream until it hits 1.0.
- New module `desktop_sync::volume`. For each managed TV where `Capabilities::VOLUME` is set on the `TisvlayTvProxy` and `desktop_sync.volume = "virtual_sink"`:
  - Create a virtual sink `tisvlay_<tv_id>` (`device.description = <Tv1.Name>`, `media.class = Audio/Sink`).
  - Subscribe to `Props` (`channelVolumes`, `mute`) on the node.
  - On change, call `TisvlayTvVolumeProxy::set_volume` / `set_muted` (already defined in [dbus_client.rs](../crates/tisvlay-core/src/dbus_client.rs)).
- Subscribe to `PropertiesChanged` on `org.tisvlay.Tv1.Volume` (reuse the existing `StateWatcher` pattern in `dbus_client.rs`). On TV-initiated change, update the virtual-sink Props so the Plasma slider moves.
- Spawn via `tokio::spawn` in [tisvlay-session/src/main.rs](../crates/tisvlay-session/src/main.rs) following the existing event-handler retry-loop pattern.
- Tear down virtual sinks on clean session exit.
- **Out of scope:** loopback routing from the virtual sink into the real HDMI sink. Documented recipe; user adds `module-loopback` themselves.

## Phase 3 — Brightness sync (Wayland external-brightness)

- `wayland-client` / `wayland-protocols` are already in [tisvlay-session/Cargo.toml:34-35](../crates/tisvlay-session/Cargo.toml) for `wayland_idle.rs` — reuse them and add the `kde-external-brightness-v1` XML under `protocols/` with `wayland-scanner` codegen.
- New module `desktop_sync::brightness`. On session start, bind `kde_external_brightness_manager_v1`. If absent, log once and skip — volume still works.
- For each managed TV where `Capabilities::BACKLIGHT` is set and an EDID is resolved:
  - Register an `external_brightness_device_v1` with EDID blob, `max_brightness = 100`, `internal = false`.
  - On `requested_brightness`, call `TisvlayTvBacklightProxy::set_backlight`.
  - Subscribe to `PropertiesChanged` on `org.tisvlay.Tv1.Backlight`; on TV-side updates call `set_observed_brightness`.

## Phase 4 — D-Bus observer fallback (read-only brightness) — DEFERRED

**Do not start, scaffold, or stub any of this until Phases 2 and 3 are merged and verified working end-to-end on real hardware.** The Wayland path is the primary integration; adding a fallback before the primary is proven risks building to a design that doesn't match what we learn from Phase 3. The `"dbus_observer"` enum variant in the `desktop_sync.brightness` config should also stay out of the Phase 1 schema until this phase is unblocked — ship only `"off" | "wayland"` initially.

When revisited:

- `desktop_sync.brightness = "dbus_observer"`.
- Subscribe to `org.kde.ScreenBrightness.BrightnessChanged`, resolve `display_dbus_name` via the `Label` property + Phase 1 map, forward to `TisvlayTvBacklightProxy::set_backlight`.
- Best-effort: no back-pressure path to KDE, so TV-initiated changes won't move the slider.

## Phase 5 — Tests, docs, UX

- `tisvlay-mock`: extend `MockState` with `volume`, `muted`, `backlight`, `hardware_id` fields and `MockHandle` setters; mirror the existing `publish_tv` / `simulate_user_accept` style.
- `tisvlay-harness` integration tests (all vendor-neutral, mock-backed):
  - KDE brightness event → `Tv1.Backlight` property set → `MockState` updated.
  - PW sink volume change → `Tv1.Volume` property set → `MockState` updated.
  - `MockState` push → `PropertiesChanged` → KDE observed-brightness / PW sink Props updated.
- New doc `docs/desktop-sync.md`: architecture, vendor-neutrality guarantees, KDE version requirements, greeter gap, per-TV config reference.
- CLI: `tisvlay desktop-sync status` prints the Phase 1 mapping table and active sync modes per TV.
- Tray: extend the per-TV submenu with a "Desktop sync" readout (mode + matched display/sink).

## Explicit Non-Goals

- Any vendor- or protocol-specific branch in desktop-sync code. If a feature needs one, it belongs in the vendor crate behind the existing `TvConnection` trait.
- Replacing `wayland_idle.rs` with KDE power events. Orthogonal; revisit after Phase 3 lands.
- Pre-login / greeter sync. Desktop-sync is user-session only.
- Shipping a kernel module (`ddcci-backlight`-style) for non-KDE brightness.
- GNOME / Sway / Hyprland brightness paths. Volume (PipeWire) already works there.
- Non-AV transports. A future CEC backend advertising `Capabilities::BACKLIGHT` + `HardwareId` would work through this feature without changes here.

## Scale & Mapping

1:1 linear, no curve adjustment, no clamping beyond the endpoints. `Capabilities::{BACKLIGHT, VOLUME}` already imply a 0–100 integer scale on the `Tv1.*` D-Bus surface, and any backend implementing those bits is required to conform to it.

- **Brightness:** KDE slider 0 % / 50 % / 100 % ↔ `Tv1.Backlight.Backlight` 0 / 50 / 100. `kde-external-brightness-v1` device advertises `max_brightness = 100`; requested values pass through untouched.
- **Volume:** PipeWire virtual-sink volume 0 % / 50 % / 100 % ↔ `Tv1.Volume.Volume` 0 / 50 / 100. Configure the sink as linear (`PW_KEY_NODE_VOLUME`-style cubic disabled) so the slider → D-Bus value is the identity, and Plasma's own cubic curve — if any — is the only curve in the chain, matching what the user sees on a native sink.
- **Mute:** boolean passthrough.

## Open Questions

- Debounce policy: trailing-edge flush at ~20 Hz to the D-Bus property setter (Plasma's scroll emits bursts). Apply uniformly across vendors. Coalesce only the intermediate values — always flush the final resting value so 100 % means 100 %.
- `HardwareId` serial field: CEC and some discovery paths can't read it — matching falls back to manufacturer + model + user override.
- Should Phase 0's `HardwareId` and the existing `DiscoveredTv.hardware_id` ship as one migration or two? Leaning one — the existing field is empty in every backend today, so there are no in-flight consumers to break.
