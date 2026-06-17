## Slint `unstable-winit-030` feature dependency

**Priority**: medium
**Found by**: GUI desktop integration (PR #54, May 2026)

### Context

`tisvlay-gui` depends on Slint's `unstable-winit-030` feature to access winit APIs from inside Slint callbacks:

- `slint::winit_030::WinitWindowAccessor::with_winit_window` — synchronous `&winit::window::Window` access from inside event-loop callbacks. Used in `dbus.rs::raise()` to call `request_user_attention` on re-activation (taskbar pulse UX on KDE Plasma 6).
- `slint::BackendSelector::with_winit_window_attributes_hook` — apply `WindowAttributes` at construction time. Used in `lib.rs` to plumb the cold-spawn `activation-token` via `WindowAttributesExtStartupNotify::with_activation_token`. Replaces an earlier `unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", ...) }` block.

Slint marks the `winit_030` module as "may be removed or changed in future minor releases" (`slint-1.16.1/lib.rs:680-743`).

### Pin

`slint = "=1.16.1"` — exact-version pin in `crates/tisvlay-gui/Cargo.toml`. The matching test dependency `i-slint-backend-testing` and the optional dev dep `i-slint-backend-selector` are similarly pinned to `=1.16.1`.

### Risk

A `slint = "1.16"` constraint would let cargo pick any future 1.16.x patch release. Slint patch releases historically don't break stable API, but `unstable-winit-030` is explicitly excluded from the stability contract — Slint reserves the right to change it within minor versions, which under cargo's resolver could mean within `1.16.x`. The exact-version pin bounds that risk.

### Mitigation options

- Keep the exact-version pin (current approach). Workspace-wide Slint upgrades become two-step: bump the pin, re-audit the `winit_030` call sites.
- Migrate to a stable Slint surface if/when slint-ui/slint#4382 lands a public raise/focus API — the `winit_030` accessor would become optional.

### Re-evaluation triggers

- Slint 1.17 or later released → audit the `winit_030` API surface; either bump the pin or re-migrate to the new path.
- `slint-ui/slint#4382` lands a public raise/focus API → drop `unstable-winit-030`, switch the re-activation path to the new API.
- `cargo update` produces a compile error despite the pin → check if a transitive Slint dep got promoted.

### Audit checklist on upgrade

When bumping `slint` to a new version, verify these still compile and behave as before:

- `crates/tisvlay-gui/src/dbus.rs::raise()` — `app.window().with_winit_window(|w| w.request_user_attention(...))` path.
- `crates/tisvlay-gui/src/lib.rs` — `BackendSelector::new().with_winit_window_attributes_hook(...)` path and the `WindowAttributesExtStartupNotify` import from `slint::winit_030::winit::platform::startup_notify`.
- `ActivationToken::from_raw(token)` import from `slint::winit_030::winit::window::ActivationToken` (note: the type is *also* exposed at `slint::winit_030::winit::platform::startup_notify::ActivationToken` but as private — use the public one).

### Alternative rejected: direct `winit = "0.30"` dev-dep

Pinning a direct winit dev-dep to Slint's vendored version sidesteps the unstable-feature warning but doesn't help: Slint's `WinitWindowAccessor` is what wraps `with_winit_window`. We don't have another entry point to reach the underlying `winit::Window` without re-implementing the backend bridge. The same Slint-version-lock is required either way.
