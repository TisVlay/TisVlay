## Slint `unstable-winit-030` feature dependency

**Priority**: medium
**Found by**: GUI desktop integration (PR #54, May 2026)

### Context

`tisvlay-gui` depends on Slint's `unstable-winit-030` feature to access winit APIs from inside Slint callbacks:

- `slint::winit_030::WinitWindowAccessor::with_winit_window` — synchronous `&winit::window::Window` access from inside event-loop callbacks. Used in `dbus.rs::raise()` to call `request_user_attention` on re-activation (taskbar pulse UX on KDE Plasma 6).
- `slint::BackendSelector::with_winit_window_attributes_hook` — apply `WindowAttributes` at construction time. Used in `lib.rs` to plumb the cold-spawn `activation-token` via `WindowAttributesExtStartupNotify::with_activation_token`. Replaces an earlier `unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", ...) }` block.

Slint marks the `winit_030` module as "may be removed or changed in future minor releases" (`api/rs/slint/lib.rs:631`).

### Pin

The dep is pinned to an exact **git rev** of `github.com/slint-ui/slint` (Slint 1.17 is not yet released on crates.io, but its built-in `Tooltip` element — used to document GUI controls — needs the 1.17 compiler). Every Slint dep across the workspace shares the identical rev: `slint`, `i-slint-backend-selector`, `i-slint-backend-testing`, and `slint-build` in `crates/tisvlay-gui/Cargo.toml`, plus `slint` + `i-slint-backend-testing` in `crates/tisvlay-harness/Cargo.toml`. A differing rev (even short vs full SHA) is a distinct cargo source and breaks the GUI↔harness `AppWindow` type identity.

Two consequences of the git pin:

- **`tisvlay-gui` is `publish = false`** for the duration — `cargo publish` rejects git/rev dependencies.
- **Workspace MSRV is `1.92`** (Slint master's floor), bumped from 1.85 in the root `Cargo.toml`.

One forced compatibility change rode in with 1.17: its new `SystemTrayIcon` element makes `i-slint-core` depend on `ksni` (under the non-optional `std` feature) with ksni's **`async-io`** backend. ksni 0.3.5 hard-errors when both `tokio` and `async-io` are enabled, and cargo unifies ksni workspace-wide — so `tisvlay-tray` was switched from ksni's default `tokio` backend to `async-io` to match. Validated at runtime against KDE Plasma's StatusNotifierHost (icon spawns, no zbus reactor panic).

### Risk

A git rev is immutable but goes **stale silently** — `cargo update` won't advance it, so security/bugfix updates to Slint require a manual rev bump. The `unstable-winit-030` module is also explicitly outside Slint's stability contract, so a rev bump can change it without warning.

### Mitigation options

- Keep the git-rev pin (current approach). Workspace-wide Slint rev bumps become two-step: bump the rev (everywhere, identically) + re-capture the Nix `outputHashes`, then re-audit the `winit_030` call sites.
- Migrate to a stable Slint surface if/when slint-ui/slint#4382 lands a public raise/focus API — the `winit_030` accessor would become optional.

### Re-evaluation triggers

- **Slint 1.17.0 tagged on crates.io** → swap the git rev back to an exact version pin (`slint = "=1.17.0"` etc.) across both Cargo.tomls, drop the Nix `outputHashes` entry, and **re-enable `tisvlay-gui` `publish = true`**.
- Advancing the pinned rev → re-audit the `winit_030` API surface, re-capture the Nix `outputHashes` (fake-hash flow), and confirm the ksni `async-io` alignment still holds.
- `slint-ui/slint#4382` lands a public raise/focus API → drop `unstable-winit-030`, switch the re-activation path to the new API.
- `cargo update` produces a compile error despite the pin → check if a transitive Slint dep got promoted.

### Audit checklist on upgrade

When bumping `slint` to a new version, verify these still compile and behave as before:

- `crates/tisvlay-gui/src/dbus.rs::raise()` — `app.window().with_winit_window(|w| w.request_user_attention(...))` path.
- `crates/tisvlay-gui/src/lib.rs` — `BackendSelector::new().with_winit_window_attributes_hook(...)` path and the `WindowAttributesExtStartupNotify` import from `slint::winit_030::winit::platform::startup_notify`.
- `ActivationToken::from_raw(token)` import from `slint::winit_030::winit::window::ActivationToken` (note: the type is *also* exposed at `slint::winit_030::winit::platform::startup_notify::ActivationToken` but as private — use the public one).

### Alternative rejected: direct `winit = "0.30"` dev-dep

Pinning a direct winit dev-dep to Slint's vendored version sidesteps the unstable-feature warning but doesn't help: Slint's `WinitWindowAccessor` is what wraps `with_winit_window`. We don't have another entry point to reach the underlying `winit::Window` without re-implementing the backend bridge. The same Slint-version-lock is required either way.
