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
- **Tray drain-loop coalescing** — [tisvlay-tray/src/main.rs:304-351](../crates/tisvlay-tray/src/main.rs): mpsc `try_recv()` loop summing `ScrollBy` deltas per (tv_id, target), flushing one D-Bus call per drain. Exact pattern to reuse for KDE-scroll → TV in Phases 2 / 3.
- **Active-session tracking** — [tisvlayd/src/logind.rs:270-377](../crates/tisvlayd/src/logind.rs) tracks logind `Active` + `class == "user"` for `on_no_session` automation. D-Bus setters are **not** caller-gated; today non-active sessions don't issue commands only because the session service runs per-user-session. Desktop-sync needs its own explicit `Session.Active` gate (Phase 2 / 3).
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
| Identity     | EDID (via `edidkit`) ↔ `Tv1.HardwareId`                | `connector` tiebreaker for same-model dual-TV setups       |
| Greeter      | Not supported (desktop-sync runs in the user session)  | Documented limitation                                      |

New code lives in `tisvlay-session` (add a `desktop_sync` module). Split into a `tisvlay-desktop-sync` sub-crate only if the module set grows beyond ~3 files. `tisvlayd` stays transport/vendor-neutral; its only additions are Phase 0.

### D-Bus Idiom for New Methods & Interfaces

All new D-Bus surfaces defined by this plan use `#[derive(zvariant::Type)]` structs rather than the tuple-of-strings + embedded-JSON pattern used by some existing methods (e.g. [`ListUserProfiles`](../crates/tisvlay-session/src/session_dbus.rs) returning `a(ss)` with JSON-encoded profiles). JSON tunnels skip D-Bus type enforcement, break client autocomplete, and are awkward to evolve. Typed structs are standard zbus usage and cost nothing extra. Existing JSON-tunneled methods are retrofit candidates — out of scope for desktop-sync, flagged in [docs/todo.md](todo.md) for later.

## Phase 0 — Vendor-agnostic identity surface (`tisvlayd` + `tisvlay-core`) — DONE

Prerequisite for mapping KDE displays / PipeWire sinks to TVs without caring which vendor a given TV is.

- Add a `HardwareId` struct to `tisvlay-core` with fields `{ manufacturer: Option<String>, model: Option<String>, serial: Option<String> }`. Manufacturer uses EDID PNPID convention (e.g. `GSM` for LG, `SAM` for Samsung) so cross-referencing EDID is mechanical.
- Add `fn hardware_id(&self) -> Option<HardwareId> { None }` to `TvConnection` ([traits.rs](../crates/tisvlay-core/src/traits.rs)) as a default-returning-`None` method — matches the existing optional-feature pattern without needing a new capability bit. Skip `HAS_HARDWARE_ID`.
- Populate the LG implementation from the existing `getSystemInfo` / `getSystemSettings` response in [tisvlay-lg/src/client.rs](../crates/tisvlay-lg/src/client.rs) (already cached; no new SSAP call required).
- Replace the unused `String hardware_id` on `DiscoveredTv` with `Option<HardwareId>`. Update [tisvlay-lg/src/discovery.rs:110-117](../crates/tisvlay-lg/src/discovery.rs) to fill it from the SSDP `X-*` headers + cached system info.
- Expose on D-Bus as a read-only `HardwareId` property on `org.tisvlay.Tv1` (struct `(sss)`, empty strings = unknown). Add a `TisvlayTvHardwareProxy` in [dbus_client.rs](../crates/tisvlay-core/src/dbus_client.rs) mirroring the existing per-interface proxies.
- Mock backend: add `hardware_id: Option<HardwareId>` to `MockState` with a setter on `MockHandle` for tests. Mirror the `publish_tv` pattern in [handle.rs:72](../crates/tisvlay-mock/src/handle.rs).

Standalone value: a future pairing wizard / GUI also wants this, independently of desktop sync.

All four changes ship in a single PR. The old `DiscoveredTv.hardware_id: String` field is empty in every backend today and has no downstream consumer, so there's no safety benefit to staging the removal behind a deprecation window.

**Landed:** One follow-up fix after initial hardware testing. The SSAP `getSystemInfo` fetch initially only fired inside `ensure_connected` (reconnect path), so the first connection returned an empty identity. Fixed by calling `get_hardware_id` right after `WebOsClient::connect` in `LgConnector::connect`, and switching `Tv1.HardwareId` to a live-read property (`manager.get_hardware_id` on each access) so late-arriving identity surfaces without re-registration. Verified on an LG OLED42C3PUA: property returns a correct `(GSM, OLED42C3PUA, <serial>)` tuple.

## Phase 1 — Display & sink identity matching (`tisvlay-session`) — DONE (display half)

The display side (DRM enumeration, EDID matching, `[tv.desktop]` persistence, `Tv1.Desktop` D-Bus surface) is implemented and verified on hardware. The sink side is deferred to Phase 2 (PipeWire enumeration lands with the volume-sync work).

- Add `edidkit` to `tisvlay-session`.
- Split the config between **identity** (per-TV, static) and **behavior** (profile-driven, activity-specific):
  ```toml
  # System config — admin baseline for sync behavior
  [user.profiles.default]
  sync_backlight = "wayland"       # none | wayland   (dbus_observer added in Phase 4)
  sync_volume    = "virtual_sink"  # none | virtual_sink

  # User config — per-activity profiles flip the toggles
  [user.profiles.gaming]
  sync_backlight = "none"          # don't let KDE dim the TV mid-game
  sync_volume    = "virtual_sink"

  # Per-TV identity — cannot live in a shared profile
  [[tv]]
  id = "living-room"
  profile.user = "gaming"
  [tv.desktop]
  edid = "abc123…"                 # EDID hash of the matching monitor; auto, overridable
  connector = "HDMI-A-2"           # optional DRM connector tiebreaker; consulted only on ambiguous EDID match
  sink = "alsa_output.…"           # PipeWire sink node name; auto, overridable
  ```
  Rationale:
  - `sync_backlight` / `sync_volume` are session-scope behavior (the sync logic runs in `tisvlay-session`, not `tisvlayd`), matching the existing `UserProfileConfig` knobs like `on_session_lock` / `idle_timeout_seconds`. Adding them to [`UserProfileConfig`](../crates/tisvlay-core/src/config.rs) (and `ResolvedUserProfile`) gives users profile-switching UX for free: `"default"` with sync on, `"gaming"` with backlight off, `"movie"` with backlight off but volume on. Admin baseline via `user.profiles.default` in system config still sets the defaults.
  - `[tv.desktop]` stays per-TV because `edid` / `sink` are matching identifiers that can't be shared across TVs. One-word section name matches `profile`/`vendor` style; identity fields sit at the section root so future non-sync consumers (tray readout, pairing wizard, input-switch automation) can reuse them.
  - Naming follows project conventions: `snake_case`, enum variants mirror `SessionAction::None`'s `"none"` rather than `"off"`, and `sync_backlight` uses the `Tv1.Backlight` interface name rather than the KDE-side term "brightness".
  - **Code defaults are `"none"` for both** when neither admin baseline nor user profile sets them. Desktop sync is novel and surprising (TV firing up its on-screen volume indicator, brightness driving the actual backlight), so opt-in by config rather than on-by-default. The example admin baseline above is intentional — admins who want sync ship-wide set it once in `[user.profiles.default]`.
- Use `edidkit` from crates.io. Match the `pipewire-native` policy: published release first, switch to a git-pinned rev only if blocked.
- **Auto-population flow.** The daemon has no DRM / PipeWire visibility (runs as system user outside a graphical session), so the session service owns auto-detection. On session service startup, scan `UserConfig` for managed TVs missing any of `edid` / `connector` / `sink`, probe the relevant system surfaces, and write back to `UserConfig` (the canonical location for `[tv.desktop]`). Client `Manager1.AddTv` takes the fields explicitly when the caller provides them; when left blank it persists the TV as usual and the session service fills `[tv.desktop]` on next run. No daemon-side probing, no new daemon D-Bus method.
- At session start, enumerate `/sys/class/drm/*/edid`, parse via `edidkit`, and match against every managed TV's `Tv1.HardwareId`. **Landed matcher:** manufacturer-anchored, not all-fields-agree. Real LG sets advertise a generic `monitor_name` descriptor (`"LG TV SSCR2"`) and a boilerplate EDID serial number (`0x01010101`), neither of which matches what SSAP `getSystemInfo` returns (`"OLED42C3PUA"` + a real unit serial). So manufacturer (the 3-letter PNPID) is the gating field; model and serial are scoring bonuses. Ranked candidates get a `connector` tiebreaker when multiple displays tie on the top score.
- **Same-model dual-TV setups:** when multiple displays match by manufacturer and tie on model/serial scoring, fall back to `[tv.desktop] connector` for disambiguation. Auto-populated from the connector the probe sees the TV on; user-overridable.
- **HDMI port swap is detected on session restart.** The probe runs only at `SessionState::load` today — there's no runtime DRM hotplug listener. If the user moves the TV to a different HDMI port, the mapping stays stale until the next session service start, at which point the new connector is persisted and pushed. Adding a udev DRM subsystem watcher is a deferred refinement; skip until a concrete user request surfaces.
- Expose the resolved mapping and sync state as a new **`org.tisvlay.Tv1.Desktop`** feature interface on each TV object, published by `tisvlayd` at `/org/tisvlay/Tv/{id}` alongside the existing per-feature interfaces ([`Tv1.Volume`](../crates/tisvlayd/src/dbus/tv.rs), `Tv1.Backlight`, etc.). Read-only properties: `Edid`, `Connector`, `Sink`, `SyncBacklight`, `SyncVolume`. Tray / GUI / CLI consume via the existing `GetManagedObjects` plumbing and get `PropertiesChanged` for free when mapping or sync state changes at runtime. No new `List*` method on `Session1`.
  - **Data flow — landed:** the session service owns both file storage under `~/.config/tisvlay/` (or the `--config` path) *and* probing; it writes `[tv.desktop]` to disk and pushes the resolved `DesktopMapping` to the daemon via the new `Manager1.SetDesktopMapping(tv_id, DesktopMapping)` RPC. The daemon holds the mapping **in-memory only** (no file I/O — the root daemon has no write access to user home directories) and republishes on `Tv1.Desktop`. Clients still read from one place. Daemon-side persistence was considered but dropped once the ownership mismatch surfaced.
  - **Persistence delta:** the probe re-runs on every session start but only rewrites the config when the resolved mapping differs from what's on disk. Idempotent restarts don't churn the file.

## Phase 2 — Volume sync (PipeWire virtual sink) — NEXT

- Use [`pipewire-native`](https://crates.io/crates/pipewire-native) from crates.io (pure-Rust, no libpipewire FFI, KDE-aligned — checkout at `/home/ruffsl/ws/kde/libs/pipewire-native-rs`). Start with the published release; only switch to a git dep with a pinned rev if we hit a blocker or need an unreleased feature. Our needs — virtual sink creation + `Props` subscribe/set, no audio streaming — fall within what the crate supports today.
- **Sink identifier stability — decide during implementation.** `node.name` is fragile (it encodes PCI paths and can change across kernel upgrades, hardware re-enumeration, or ALSA card reordering). More durable options: `device.description` + connector for HDMI outputs, `device.serial` for USB DACs, or a compound `(card_name, connector)` key. Pick whichever most-stable identifier `pipewire-native` exposes cleanly when we get there, and store the chosen field in `[tv.desktop] sink` along with a small discriminator so the session service knows what to match against on the next run. Do **not** hard-code `node.name` in the auto-detection probe without revisiting this.
- New module `desktop_sync::volume`. For each managed TV where `Capabilities::VOLUME` is set on the `TisvlayTvProxy` and the effective user profile has `sync_volume = "virtual_sink"`:
  - Create a virtual sink `tisvlay_<tv_id>` (`device.description = <Tv1.Name>`, `media.class = Audio/Sink`).
  - Subscribe to `Props` (`channelVolumes`, `mute`) on the node.
  - On change, enqueue a `Set{Volume,Muted}` event to an mpsc channel; a drain-loop task coalesces per-TV bursts (mirror the tray pattern at [tisvlay-tray/src/main.rs:304-351](../crates/tisvlay-tray/src/main.rs)) and flushes one `TisvlayTvVolumeProxy::set_volume` / `set_muted` call per drain. Mute goes through the same channel but doesn't sum — last-write-wins.
- Subscribe to `PropertiesChanged` on `org.tisvlay.Tv1.Volume` (reuse the existing `StateWatcher` pattern in `dbus_client.rs`). On TV-initiated change, update the virtual-sink Props directly — no coalescing needed, TV pushes are sparse (remote presses, app-driven changes).
- Spawn via `tokio::spawn` in [tisvlay-session/src/main.rs](../crates/tisvlay-session/src/main.rs) following the existing event-handler retry-loop pattern.
- **Active-session gate:** subscribe to the session's own logind `Session.Active` property. When `Active = false`, stop flushing desktop → TV events (protects against fast-user-switch races where two session services briefly coexist). Leave the virtual sink registered so audio routing stays stable; we just pause the bridge.
- **Profile-flip teardown:** when the resolved user profile's `sync_volume` transitions to `"none"`, **tear down the virtual sink** so KDE's volume widget falls back to the user's default HDMI sink natively — mimicking the no-tisvlay UX. Re-create on transition back to `"virtual_sink"`.
- **Disconnect teardown:** when `Tv1.Connected` transitions to `false`, tear down the virtual sink too. Rationale: the user opted into sync to route volume at the TV, so silently falling back to software-level sink control would hide the disconnection; the TV's own on-screen volume indicator also stops echoing, which is the expected feedback on most devices. Re-create on reconnect. `Tv1.Connected` is already debounced upstream by the daemon's reconnect logic, so no additional hysteresis needed here.
- **Initial-state direction at session start:** desktop-wins. On first registration, read the last-known virtual-sink volume from PipeWire / KDE memory and push it to the TV via `set_volume`. Covers transitions like "someone watched a movie last night at 80 %; I sit down at the PC this morning and expect my desktop's saved levels, not last night's." Same rationale as the time-of-day brightness profile common on Linux desktops.
- Tear down virtual sinks on clean session exit.
- **Out of scope:** loopback routing from the virtual sink into the real HDMI sink. Documented recipe; user adds `module-loopback` themselves.

## Phase 3 — Brightness sync (Wayland external-brightness)

- `wayland-client` / `wayland-protocols` are already in [tisvlay-session/Cargo.toml:34-35](../crates/tisvlay-session/Cargo.toml) for `wayland_idle.rs` — reuse them and add the `kde-external-brightness-v1` XML under `protocols/` with `wayland-scanner` codegen.
- New module `desktop_sync::brightness`. On session start, bind `kde_external_brightness_manager_v1`. If absent, log once and skip — volume still works.
- For each managed TV where `Capabilities::BACKLIGHT` is set, an EDID is resolved, and the effective user profile has `sync_backlight = "wayland"`:
  - Register an `external_brightness_device_v1` with EDID blob, `max_brightness = 100`, `internal = false`.
  - On `requested_brightness`, enqueue to an mpsc channel; drain-loop coalesces bursts (same tray pattern) and flushes one `TisvlayTvBacklightProxy::set_backlight` per drain.
  - Subscribe to `PropertiesChanged` on `org.tisvlay.Tv1.Backlight`; on TV-side updates call `set_observed_brightness` directly (no coalescing — TV pushes are sparse).
- **Active-session gate:** same `Session.Active` check as Phase 2 — pause desktop → TV flushes when inactive; leave the Wayland device registered so KDE's slider state is preserved.
- **Profile-flip teardown:** when `sync_backlight` transitions to `"none"`, **unregister the `external_brightness_device_v1`** so KDE's brightness widget reverts to its software-brightness path for that display — mimicking the no-tisvlay UX. Re-register on transition back to `"wayland"`.
- **Disconnect teardown:** when `Tv1.Connected` transitions to `false`, unregister the Wayland device too. The slider going unresponsive is the intended signal — with sync enabled, the user expects hardware backlight control, and silently falling back to software brightness would mask the disconnection. Re-register on reconnect.
- **Initial-state direction at session start:** desktop-wins. On first registration, call `set_observed_brightness` to the KDE slider's last-known value, then immediately forward it to the TV via `set_backlight`. Handles the "movie last night → monitor this morning" transition and respects the desktop's time-of-day brightness profiles.

## Phase 4 — D-Bus observer fallback (read-only brightness) — DEFERRED

**Do not start, scaffold, or stub any of this until Phases 2 and 3 are merged and verified working end-to-end on real hardware.** The Wayland path is the primary integration; adding a fallback before the primary is proven risks building to a design that doesn't match what we learn from Phase 3. The `"dbus_observer"` enum variant for `sync_backlight` should also stay out of the Phase 1 schema until this phase is unblocked — ship only `"none" | "wayland"` initially.

When revisited:

- `sync_backlight = "dbus_observer"` in the user profile.
- Subscribe to `org.kde.ScreenBrightness.BrightnessChanged`, resolve `display_dbus_name` via the `Label` property + Phase 1 map, forward to `TisvlayTvBacklightProxy::set_backlight`.
- Best-effort: no back-pressure path to KDE, so TV-initiated changes won't move the slider.

## Phase 5 — Tests, docs, UX

- `tisvlay-mock`: `hardware_id` + setter already landed in Phase 0. Extend `MockState` with `volume`, `muted`, `backlight` fields and `MockHandle` setters the same way; mirror the existing `publish_tv` / `simulate_user_accept` style.
- Unit-level mocks only for now — no headless PipeWire daemon / fake KDE external-brightness server in CI. Bulk of verification happens via live iteration on the dev machine to confirm the approach works at all; wire integration tests to `tisvlay-harness` once the feature is proven on hardware.
- Extend [docs/configs/daemon.toml](configs/daemon.toml) and [docs/configs/session.toml](configs/session.toml) with example profiles that toggle `sync_backlight` / `sync_volume` (e.g. a `"gaming"` profile with backlight off + volume on, a `"movie"` profile with both off). Reuse the existing CLI profile subcommands (`tisvlay profile ...`) and tray profile selector for switching during development — no new CLI surface needed.
- New doc `docs/desktop-sync.md`: architecture, vendor-neutrality guarantees, KDE version requirements, greeter gap, per-TV config reference, same-model-dual-TV `connector` tiebreaker.
- Tray: append `(sync)` to the brightness and volume readouts when the respective sync is enabled for that TV (tri-state icon variants deferred — iterate once live).

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

- **Sink identifier stability** (for Phase 2): `node.name` encodes PCI paths and changes across kernel upgrades or hardware re-enumeration. More durable candidates: `device.description` + connector for HDMI sinks, `device.serial` for USB DACs, or a compound `(card_name, connector)` key. Pick the most-stable identifier `pipewire-native` exposes cleanly when implementing the probe, and record which field was chosen so the next-run matcher knows what to compare against. Do **not** hard-code `node.name` in the probe without revisiting this.
- **Live DRM hotplug detection** (deferred from Phase 1): today the probe only re-runs at session start, so HDMI port swaps surface only after a session restart. A udev DRM subsystem watcher would pick this up live. Skip until a concrete user request surfaces or until we're building the GUI pairing wizard that would benefit from live feedback.
- **Same-model dual-TV swap** (Phase 1 corner case): two identical LG TVs on the same host can't be disambiguated by identity alone — both EDIDs report the same generic `monitor_name` and a boilerplate serial. If the user physically swaps ports between the two, the matcher will keep the old connector hints and silently assign the mappings to the wrong TVs. Documented workaround: user hand-edits `[tv.desktop].connector` in the user config, or powers one TV off during a probe pass. Fancier fix (require the user to confirm via an interactive pairing wizard) stays out of scope for now.
