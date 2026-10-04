# GUI test coverage fan-out (Layer-C end-to-end tests)

The testing-infra sprint shipped one canonical Layer-C test (`tisvlay-harness/tests/gui_pair_flow.rs`) as a proof-of-concept for the [`TestGui` fixture](../testing.md#gui-testing-testgui--i-slint-backend-testing). The implicit follow-on — a fan-out of end-to-end tests covering the rest of the GUI's flows — was scoped out so the first PR didn't sprawl. The coverage report shows the gap concretely:

| Module | LoC | Coverage | What's untested |
|---|---|---|---|
| `lib.rs` | 1803 | 0% | `register_callbacks` body — every `Daemon.on_set_volume`, `Daemon.on_pair_tv`, `EditPane.on_save`, etc. |
| `state_watch.rs` | 449 | 0% | `watch_state` event branches (DaemonAppeared/Disappeared, refresh-on-state-change, fallback poll) |
| `wizard.rs` | 305 | 0% | discovery, schema prefetch, pair flow, per-field error routing |
| `about.rs` | 218 | 0% | `make_about_components` rendering path + `drift_tone` |
| `edit_pane.rs` | 81 | 0% | edit-form save flow |
| `power_cycle.rs` | 70 | 0% | `run_power_on_with_progress` |
| `toast.rs` | 35 | 0% | `show_toast` (technically Layer-B testable) |
| `accent.rs` | 70 | 0% | xdg-desktop-portal accent sync (hard — needs portal mocking) |
| `icon_theme.rs` | 64 | 0% | freedesktop icon theme detection (hard — fs-dependent) |

Realistic target with a focused fan-out: workspace line coverage from today's 52.41% → 60–65%.

## Gating condition

CI build time. The fan-out was first planned as one test binary per flow, each linking the full GUI dep graph (slint, ashpd, arboard, freedesktop-icons, etc.). That premise no longer holds: Slint's testing backend initializes once per process behind the fixture's `OnceLock` guard, so tests share a binary, and gui-app's suites already live in one (`tests/it/`, see [testing.md](../testing.md) "One integration-test binary per crate"). New flows add modules, not binaries. What remains is the runtime of booting `TestGui` per scenario — and the previous PR already hit a runner-OOM during link that we worked around by dropping `--all-features` from the coverage build.

**Pick this up after CI build setup is optimized.** Candidate optimizations to land first:

- **`mold` or `lld` linker** in `.cargo/config.toml`. Slashes link time and meaningfully reduces peak link memory. Workspace-wide change, low risk.
- **`CARGO_PROFILE_DEV_DEBUG=line-tables-only`** for coverage builds. Cuts object-file size significantly while keeping enough debug info for `llvm-cov` region mapping. Slot into `_cov-collect` and `test-ci` recipes.
- **Larger CI runner** or self-hosted runner. Paid; trade-off with the speed-vs-cost story.
- **Layer-B-first refactor.** Some scenarios listed below could be exercised at Layer B (no event loop, no full `TestGui` boot) if `register_callbacks` were broken into per-pane modules with a testable public API. Cheaper at runtime; bigger refactor up-front.

## Scenarios to fan out

Order from highest-coverage-gain to lowest:

1. **Volume / mute / backlight / screen-toggle round-trips** — exercise the slider + toggle callbacks in `register_callbacks` (lib.rs's biggest 0% chunk). Each test drives `app.global::<Daemon>().invoke_set_volume(...)`, asserts the daemon got the call via `MockHandle::expect_volume`, mock acknowledges, AppWindow reflects the new value.
2. **Edit-mode form** — open edit pane → mutate a field → save → assert daemon's `UpdateConfig` got called → assert form closes. Hits `edit_pane.rs` + the relevant `register_callbacks` branch.
3. **Pairing wizard happy path** — discover → select → enter creds → pair → see TV. Hits `wizard.rs` end-to-end.
4. **Pairing wizard per-field error** — daemon returns `ConfigInvalidVendorConfig`; assert form bounces back to step 2 with inline-error on the offending field. Hits `wizard.rs::mark_field_invalid` and `edit_pane::extract_invalid_vendor_config`.
5. **Power-on with progress** — exercises `power_cycle.rs::run_power_on_with_progress` end-to-end.
6. **Daemon disappear/reappear** — exercises `state_watch.rs` event branches.
7. **Diagnostic-button click round-trip** — Layer-B was passive; this clicks. Exercises `about.rs::build_diagnostic_blob`.

## Refactors required

- **Expose `register_callbacks` via `__internals`** — tests need to invoke it on a synthesized AppWindow + mocked DaemonClient, mirroring the existing `__internals` re-export pattern (`SnapshotInputs`, `convert_snapshot`, etc.).
- **Split `run()` into `bootstrap()` + `run_loop()`** — tests call `bootstrap` (build app, wire callbacks, return the AppWindow) without entering the blocking event loop. Production main calls both.
- **Per-scenario modules under `tests/it/`** — one module per flow in gui-app's single test binary. The fixture's `OnceLock` guard makes Slint's once-per-process init safe to share.

## What `accent` and `icon_theme` need

Both modules are filesystem- or portal-dependent and don't fit the in-process `TestGui` model cleanly:

- **`accent.rs`** — xdg-desktop-portal accent-color sync. Testing requires a fake portal exposing `org.freedesktop.appearance.accent-color`. Possible via a `PrivateBus` + a portal-shaped fake (similar to `FakeScreenSaver`) but adds a sibling fake to `tisvlay-test`. Defer until a regression in the live portal path surfaces.
- **`icon_theme.rs`** — reads `~/.config/kdeglobals` and walks freedesktop icon theme dirs. Testing in-process requires an env-var override for `$HOME` (already handled via `std::env::var_os` in the implementation) plus a tempdir fixture seeded with synthetic icons. Tractable but low-payoff — the function is small and rarely changes.
