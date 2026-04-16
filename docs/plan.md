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
| Brightness   | Virtual backlight kernel module (`/sys/class/backlight/tisvlay-*`) | `org.kde.ScreenBrightness` D-Bus observer (no hardware control) |
| Volume       | Bind to existing HDMI sink per TV via `pipewire-native` | ELD → `HardwareId` matching reuses Phase 1 identity logic  |
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
  sync_volume    = "mirror_hdmi"   # none | mirror_hdmi

  # User config — per-activity profiles flip the toggles
  [user.profiles.gaming]
  sync_backlight = "none"          # don't let KDE dim the TV mid-game
  sync_volume    = "mirror_hdmi"

  # Per-TV identity — cannot live in a shared profile
  [[tv]]
  id = "living-room"
  profile.user = "gaming"
  [tv.desktop]
  edid = "abc123…"                 # EDID hash of the matching monitor; auto, overridable
  connector = "HDMI-A-2"           # optional DRM connector tiebreaker; consulted only on ambiguous EDID match
  sink = "alsa_output.…"           # PipeWire sink node name; auto-resolved each session from ELD; overridable
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

## Phase 2 — Volume sync (mirror HDMI sink) — DONE

Ships the `mirror_hdmi` mode only; no virtual sink created. The existing PipeWire HDMI sink that corresponds to the TV becomes the slider the user already knows, and we bridge its `Props.channelVolumes` / `Props.mute` with `Tv1.Volume`. Early prototyping with a separate `virtual_sink` mode was dropped — it added an extra row in KDE's audio widget that the user had to manually route to, and audio continued flowing through the real HDMI sink anyway. Mirror mode keeps the native output row and sidesteps that friction.

- Use [`pipewire-native`](https://crates.io/crates/pipewire-native) pinned to upstream git rev `0a2cff2e73ba79f63568fb892073b264266714a2`. The 0.1.4 crates.io release aborts inside an FFI callback when the PipeWire 1.4.x protocol hits a decode error (fixed upstream but not yet in a release tag). Revert to a crates.io pin when 0.1.5+ ships.
- `tisvlay-session/src/volume_sync.rs` owns a `ThreadLoop`, `Core`, and `Registry`. The Context is held alongside Core in the same `Inner` struct — `Core` holds only a `Weak<Context>` internally, and letting Context drop crashes later callbacks.
- **Sink discovery.** On connect we install a `RegistryEvents::global` listener that filters to `media.class = "Audio/Sink"` globals. For each we parse `api.alsa.card` + `api.alsa.pcm.device`, read `/proc/asound/card$N/eld#$N.$M`, and derive a [`HardwareId`] from the ELD (manufacture_id → PNPID, monitor_name → model; ELD doesn't carry a serial so we use `None`). Cached in an `Inner.candidates: HashMap<global_id, SinkCandidate>`. The `global_remove` callback keeps the cache current.
- **Matching** reuses `desktop_probe::identity_matches` + `match_score` so the manufacturer-anchored EDID matcher behavior is identical across display and audio probes. When multiple candidates tie on score, first wins — same-model-dual-HDMI-port disambiguation relies on `[tv.desktop].connector` today (the ELD reported by each PCM is usually identical across ports of the same model).
- **Binding.** Per TV: `Registry::bind(global_id, NODE, 3)` returns a typed `Node`. We `subscribe_params(&[ParamType::Props])`, attach a param listener, and track the binding in `Inner.bindings: HashMap<tv_id, Binding>`. Unbind drops the proxy — we do **not** call `Core::destroy` (the sink isn't ours).
- **Forward path** (user scrolls KDE slider): the listener callback parses `Props.channelVolumes` / `Props.mute`, suppresses values equal to the last-pushed cache entry, and sends `SinkEvent`s to a `tokio::sync::mpsc::UnboundedSender`. A tokio drain-loop coalesces bursts per TV and calls `DaemonClient::set_volume` / `set_mute` once per drain. Mute is last-write-wins, not coalesced further.
- **Reverse path** (TV remote changes volume): the coordinator subscribes to `StateWatcher`. On `PropertiesChanged` for a TV we `push_tv_state(tv_id, volume, muted)` which writes the volume through the PipeWire **Device Route**, not the Node Props. Hardware ALSA sinks have a two-layer volume model: KDE and WirePlumber read/write the Device Route, while `Node::set_param(Props)` only updates a lower layer that KDE doesn't observe. The fix binds the owning Device proxy (discovered from the Node's `device.id` info prop), subscribes to `ParamType::Route` to learn the route index for each output (keyed by `card.profile.device`), and calls `Device::set_param(Route)` with a nested Props pod containing `channelVolumes` + `mute`. The nested pod is pre-serialised as a `RawPodOwned` because `ObjectBuilder::push_property` only accepts flat `Pod` values.
- **Volume scale.** PipeWire's `channelVolumes` is cubic (`cv^3` is the real gain) and Plasma displays percent as `cbrt(cv) * 100`. Our host-side `u8 Volume` is linear 0-100, so we cube on write and cube-root on read. 50 KDE ↔ 50 TV, 100 ↔ 100.
- **Coordinator** reconciles every 10 s and on every `StateWatcher` event. For each managed TV with `sync_volume = "mirror_hdmi"` + `Connected = true` + non-empty `HardwareId`, it picks the best candidate sink from the registry cache and binds. TVs that lose connection, flip to `sync_volume = "none"`, or whose candidate sink disappeared get unbound in the same pass.
- **Opt-in via `TISVLAY_VOLUME_SYNC=1`** until real-hardware verification is stable. The pipewire-native git pin works against PipeWire 1.4.10 in practice but the crate is pre-1.0 with acknowledged API churn; keeping the env gate avoids surprising users if a protocol-decode regression re-lands upstream.
- **Known follow-ups** (out of scope for the first-landing Phase 2 commit):
  - Active-session gate against logind `Session.Active` (pause forward flushes on inactive sessions without unbinding). Not wired yet; in practice the session service only runs per-user-session so the race window is small.
  - Same-model duals on different HDMI ports when the ELD reports the same manufacturer + monitor name. Today the first candidate wins; a follow-up should correlate `api.alsa.pcm.device` with `[tv.desktop].connector` for disambiguation.
  - Flip the opt-in default once the pipewire-native pin lands in a 0.1.5+ crates.io release.

## Phase 3 — Brightness sync (virtual backlight kernel module) — PLANNING

The Wayland `kde-external-brightness-v1` approach was implemented and reverted (commits d0f9a8c / a072773). The protocol only intercepts displays claimed by `BacklightDetector` (sysfs) or `DDCutilDetector` (DDC/CI) — it explicitly skips displays where KWin provides software brightness (`KWinDisplayDetector`). The LG TV has no sysfs backlight and DDC/CI is unresponsive (`ddcutil` confirms "I2C slave address x37 is unresponsive"), so KWin claims it with software brightness (MaxBrightness=10000) and the external protocol is locked out.

The correct approach: a virtual backlight kernel module that creates `/sys/class/backlight/tisvlay-*` entries per managed TV — the same pattern `ddcci-backlight` uses for DDC/CI monitors. With a real sysfs backlight device, `BacklightDetector` claims the TV, KWin stops applying software brightness, and the user gets hardware-only backlight control (no wasted energy driving the OLED panel at full brightness while software dims the alpha channel). The daemon translates sysfs writes to `Tv1.Backlight` commands over the network.

See [docs/plan2.md](plan2.md) for the full virtual backlight sub-plan.

## Phase 4 — Tests, docs, UX (renumbered from Phase 5)

- `tisvlay-mock`: `hardware_id` + setter already landed in Phase 0. Extend `MockState` with `volume`, `muted`, `backlight` fields and `MockHandle` setters the same way; mirror the existing `publish_tv` / `simulate_user_accept` style.
- Unit-level mocks only for now — no headless PipeWire daemon / fake KDE external-brightness server in CI. Bulk of verification happens via live iteration on the dev machine to confirm the approach works at all; wire integration tests to `tisvlay-harness` once the feature is proven on hardware.
- Extend [docs/configs/daemon.toml](configs/daemon.toml) and [docs/configs/session.toml](configs/session.toml) with example profiles that toggle `sync_backlight` / `sync_volume` (e.g. a `"gaming"` profile with backlight off + volume on, a `"movie"` profile with both off). Reuse the existing CLI profile subcommands (`tisvlay profile ...`) and tray profile selector for switching during development — no new CLI surface needed.
- New doc `docs/desktop-sync.md`: architecture, vendor-neutrality guarantees, KDE version requirements, greeter gap, per-TV config reference, same-model-dual-TV `connector` tiebreaker.
- Tray: append `(sync)` to the brightness and volume readouts when the respective sync is enabled for that TV (tri-state icon variants deferred — iterate once live).

## Explicit Non-Goals

- Any vendor- or protocol-specific branch in desktop-sync code. If a feature needs one, it belongs in the vendor crate behind the existing `TvConnection` trait.
- Replacing `wayland_idle.rs` with KDE power events. Orthogonal; revisit after Phase 3 lands.
- Pre-login / greeter sync. Desktop-sync is user-session only.
- ~~Shipping a kernel module (`ddcci-backlight`-style) for non-KDE brightness.~~ Revisited: a virtual backlight kernel module is now the Phase 3 approach — see [plan2.md](plan2.md).
- GNOME / Sway / Hyprland brightness paths. Volume (PipeWire) already works there.
- Non-AV transports. A future CEC backend advertising `Capabilities::BACKLIGHT` + `HardwareId` would work through this feature without changes here.

## Scale & Mapping

1:1 linear, no curve adjustment, no clamping beyond the endpoints. `Capabilities::{BACKLIGHT, VOLUME}` already imply a 0–100 integer scale on the `Tv1.*` D-Bus surface, and any backend implementing those bits is required to conform to it.

- **Brightness:** KDE slider 0 % / 50 % / 100 % ↔ `Tv1.Backlight.Backlight` 0 / 50 / 100. The virtual backlight kernel module advertises `max_brightness = 100`; sysfs writes pass through to the daemon untouched.
- **Volume:** bound HDMI sink slider 0 % / 50 % / 100 % ↔ `Tv1.Volume.Volume` 0 / 50 / 100. PipeWire's `channelVolumes` is cubic and Plasma displays percent as `cbrt(cv) * 100`; we cube on write and cube-root on read so the linear 0-100 value on the `Tv1.Volume` D-Bus surface matches what Plasma shows.
- **Mute:** boolean passthrough.

## Open Questions

- **Sink identifier stability** (resolved in Phase 2): the registry-driven approach sidesteps this entirely. We don't persist a sink identifier across sessions — every session start re-enumerates PipeWire Audio/Sink globals via the `Registry` listener, matches each to a TV by ELD-derived `HardwareId`, and binds fresh. `node.name` changes across kernel upgrades are invisible to us because we never store them. `[tv.desktop].sink` still exists in the config schema for manual overrides and tray-readout consumers, but the matcher doesn't need it.
- **Live DRM hotplug detection** (deferred from Phase 1): today the probe only re-runs at session start, so HDMI port swaps surface only after a session restart. A udev DRM subsystem watcher would pick this up live. Skip until a concrete user request surfaces or until we're building the GUI pairing wizard that would benefit from live feedback.
- **Same-model dual-TV swap** (Phase 1 corner case): two identical LG TVs on the same host can't be disambiguated by identity alone — both EDIDs report the same generic `monitor_name` and a boilerplate serial. If the user physically swaps ports between the two, the matcher will keep the old connector hints and silently assign the mappings to the wrong TVs. Documented workaround: user hand-edits `[tv.desktop].connector` in the user config, or powers one TV off during a probe pass. Fancier fix (require the user to confirm via an interactive pairing wizard) stays out of scope for now.
