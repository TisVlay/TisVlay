## Slint `unstable-winit-030` feature dependency

**Priority**: medium
**Found by**: GUI desktop integration (PR #54, May 2026)

### Context

`tisvlay-gui` depends on Slint's `unstable-winit-030` feature to access winit APIs from inside Slint callbacks:

- `slint::winit_030::WinitWindowAccessor::with_winit_window` — synchronous `&winit::window::Window` access from inside event-loop callbacks. Used in `dbus.rs::raise()` to call `request_user_attention` on re-activation (taskbar pulse UX on KDE Plasma 6).
- `slint::BackendSelector::with_winit_window_attributes_hook` — apply `WindowAttributes` at construction time. Used in `lib.rs` to plumb the cold-spawn `activation-token` via `WindowAttributesExtStartupNotify::with_activation_token`. Replaces an earlier `unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", ...) }` block.

Slint marks the `winit_030` module as "may be removed or changed in future minor releases" (its module doc in `slint`'s `lib.rs`).

### Pin

`slint = "=1.18.1"` — exact-version pin in `crates/tisvlay-gui/Cargo.toml`. The matching test dependency `i-slint-backend-testing` and `slint-build` are similarly exact-pinned to `=1.18.1` (the latter so the `.slint` compiler can't desync from the runtime under `cargo update`).

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

- `crates/tisvlay-gui/src/dbus.rs::raise()` — `app.window().with_winit_window(|w| w.request_user_attention(...))` path.
- `crates/tisvlay-gui/src/lib.rs` — `BackendSelector::new().with_winit_window_attributes_hook(...)` path and the `WindowAttributesExtStartupNotify` import from `slint::winit_030::winit::platform::startup_notify`.
- `ActivationToken::from_raw(token)` import from `slint::winit_030::winit::window::ActivationToken` (note: the type is *also* exposed at `slint::winit_030::winit::platform::startup_notify::ActivationToken` but as private — use the public one).

### Alternative rejected: direct `winit = "0.30"` dev-dep

Pinning a direct winit dev-dep to Slint's vendored version sidesteps the unstable-feature warning but doesn't help: Slint's `WinitWindowAccessor` is what wraps `with_winit_window`. We don't have another entry point to reach the underlying `winit::Window` without re-implementing the backend bridge. The same Slint-version-lock is required either way.
