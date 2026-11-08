## Slint `unstable-winit-030` feature dependency

**Priority**: medium
**Found by**: GUI desktop integration (PR #54, May 2026)

### Context

`tisvlay-gui` depends on Slint's `unstable-winit-030` feature to access winit APIs from inside Slint callbacks:

- `slint::winit_030::WinitWindowAccessor::with_winit_window` — synchronous `&winit::window::Window` access from inside event-loop callbacks. Used in `dbus.rs::raise()` to call `request_user_attention` on re-activation (taskbar pulse UX on KDE Plasma 6).
- `slint::BackendSelector::with_winit_window_attributes_hook` — apply `WindowAttributes` at construction time. Used in `lib.rs` to plumb the cold-spawn `activation-token` via `WindowAttributesExtStartupNotify::with_activation_token`. Replaces an earlier `unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", ...) }` block.
- `slint::winit_030::WinitWindowAccessor::on_winit_window_event` — see a winit `WindowEvent` before Slint does and `Propagate` or `PreventDefault` it. Used in `lib.rs` for the mouse's Back and Forward buttons (history), for `ScaleFactorChanged` (couch mode multiplies the compositor's scale and prevents the original) and `Resized` (couch mode's scale goes on at the window's first size, since Slint creates the window when the loop starts and sets its scale past the hook; the compositor's scale is read through `with_winit_window`, because the `&slint::Window` the hook is handed reports the scale already applied), and for `KeyboardInput`: the keys the backend would drop are remapped (`keys::remap_dropped_key`, matching `winit::keyboard::{Key, NamedKey, NativeKey}`) and dispatched as Slint keys, and a navigation key is wrapped in the focus-loss watch (`is_navigation_key`, `watch_focus_after`). The model is in [gui/remote-navigation.md](../gui/remote-navigation.md).

Slint marks the `winit_030` module as "may be removed or changed in future minor releases" (its module doc in `slint`'s `lib.rs`).

### Pin

`slint = "=1.18.1"` — exact-version pin in the root `Cargo.toml`'s `[workspace.dependencies]`, shared by `tisvlay-gui` and `tisvlay-gui-app`. The matching test dependency `i-slint-backend-testing` and `slint-build` are similarly exact-pinned to `=1.18.1` (the latter so the `.slint` compiler can't desync from the runtime under `cargo update`).

### Risk

A caret `slint = "1.17"` constraint would let `cargo update` pick any future 1.17.x — or a later minor. Slint patch releases historically don't break stable API, but `unstable-winit-030` is explicitly excluded from the stability contract — Slint reserves the right to change it within minor versions. The exact pin bounds that risk; a committed `Cargo.lock` adds reproducibility but doesn't replace the pin — it only governs what a `cargo update` may move to.

### Mitigation options

- Keep the exact-version pin (current approach). Workspace-wide Slint upgrades become two-step: bump the pin, re-audit the `winit_030` call sites.
- Migrate to a stable Slint surface if/when slint-ui/slint#4382 lands a public raise/focus API — the `winit_030` accessor would become optional.

### Re-evaluation triggers

- A new Slint minor (1.18+) released → audit the `winit_030` API surface; either bump the pin or re-migrate to the new path.
- `slint-ui/slint#4382` lands a public raise/focus API → drop `unstable-winit-030`, switch the re-activation path to the new API.
- `cargo update` produces a compile error despite the pin → check if a transitive Slint dep got promoted.

### On 1.17.0 — ksni coupling resolved upstream

1.17's new `SystemTrayIcon` made `i-slint-core` pull `ksni` on a hardcoded
`async-io` backend, which would clash (Cargo feature unification) with
`tisvlay-tray`'s default-tokio ksni. We filed and landed the upstream fix —
[slint-ui/slint#12141](https://github.com/slint-ui/slint/pull/12141), gating the
tray/ksni backend behind a `system-tray` Cargo feature, released in 1.17.0. So
`tisvlay-gui` builds `default-features = false` without `system-tray`, Slint pulls
no `ksni`, and `tisvlay-tray` keeps its tokio backend.

**1.16.1 → 1.17.0 audit (done).** The three `winit_030` call sites below compile
unchanged against 1.17.0 (`just check` green); the module is still present and its
API surface (`WinitWindowAccessor::with_winit_window`,
`with_winit_window_attributes_hook`, the `activation-token` plumbing) is intact.
Behavioral checks (taskbar pulse on re-activation, startup-notify activation
token) are manual KDE QA.

**1.17.0 → 1.18.1 audit (done).** The `winit_030` module still wraps winit 0.30 and the three call sites compile unchanged (`just check` green). 1.18 deprecates the `ScrollView` / `Flickable` `viewport-*` properties as aliases of `content-*`; the UI now uses `content-width` / `content-y`. Behavioral changes worth a manual pass: `Slider` no longer emits `changed` when already at min/max (the volume/backlight `OptimisticSlider` at 0 and 100), focus clears when the focused item turns invisible, and the winit/Wayland first-frame-size and fractional-scaling fixes.

### Audit checklist on upgrade

When bumping `slint` to a new version, verify these still compile and behave as before:

- `crates/tisvlay-gui-app/src/dbus.rs::raise()` — `app.window().with_winit_window(|w| w.request_user_attention(...))` path.
- `crates/tisvlay-gui-app/src/lib.rs` — `BackendSelector::new().with_winit_window_attributes_hook(...)` path and the `WindowAttributesExtStartupNotify` import from `slint::winit_030::winit::platform::startup_notify`.
- `ActivationToken::from_raw(token)` import from `slint::winit_030::winit::window::ActivationToken` (note: the type is *also* exposed at `slint::winit_030::winit::platform::startup_notify::ActivationToken` but as private — use the public one).
- `crates/tisvlay-gui-app/src/lib.rs` — the `on_winit_window_event` hook: `EventResult`, `WindowEvent::{MouseInput, ScaleFactorChanged, KeyboardInput}`, `ElementState`, `MouseButton`, and the key event's `logical_key`, `state` and `repeat` fields.
- `crates/tisvlay-gui-app/src/keys.rs` — `remap_dropped_key` and its test, matching `NamedKey::BrowserBack` and `NamedKey::Close`, and the `XF86MenuKB` and `XF86Info` keysyms as `NativeKey::Xkb`. A behavioral check needs Plasma Bigscreen and a gamepad (Square opens the focused row's help, Share backs out) or a TV forwarding its remote's keys (Exit backs out, Info opens help).
- `crates/tisvlay-gui-app/src/lib.rs` — `compositor_scale_now`: `with_winit_window(|w| w.scale_factor())`.

### Alternative rejected: direct `winit = "0.30"` dev-dep

Pinning a direct winit dev-dep to Slint's vendored version sidesteps the unstable-feature warning but doesn't help: Slint's `WinitWindowAccessor` is what wraps `with_winit_window`. We don't have another entry point to reach the underlying `winit::Window` without re-implementing the backend bridge. The same Slint-version-lock is required either way.
