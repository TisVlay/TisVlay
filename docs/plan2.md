# GUI testing infra + accessibility + .slint modularization

> **Status:** scoped. PR ladder lives in "Sequencing" below; follow-on work that doesn't fit in the first PR is tabled in [`docs/ideas.md`](ideas.md). Schema reference for the diagnostic blob (last sprint) is now [`docs/diagnostics.md`](diagnostics.md).

`tisvlay-gui` ships the diagnostic-blob enrichment (PRs `2056dc3` / `d7305d8` / `1f7ee0f`) but its own test surface is thin — only the `diagnostic` module + a handful of `file_manager` tests have direct coverage. Before resuming the original GUI roadmap (M3.5 profile CRUD, M4 profiles pane, M5 session pane polish in [`docs/plan.md`](plan.md)), this sprint shores up testing infra so subsequent feature work has somewhere to land regression-prevention assertions.

Three threads bundle into one sprint because they're mutually-enabling: tests need accessibility labels to find widgets, accessibility labels enable real screen-reader support if we can re-enable Slint's `accessibility` feature, and modularizing the larger `.slint` files makes both the label audit and future test additions easier to scope file-by-file.

## Goals

1. **End-to-end testable GUI in headless CI** — boot an `AppWindow` against a real `tisvlay-harness::TestSession` (private dbus + daemon + session + mock TV backend), drive interactions, assert state. No display server, no GPU.
2. **Accessibility-labeled widgets** — every interactive element (buttons, list rows, pane-nav chips, edit-mode triplet, pair flow, etc.) carries the right pair of `accessible-*` properties: `accessible-label` for screen readers (translated via `@tr(...)`), and `accessible-id` for stable test selectors (literal, untranslated). Upstream split per [slint-ui/slint#9940](https://github.com/slint-ui/slint/pull/9940), merged 2025-11-05; the maintainer there explicitly recommends `accessible-id` over `accessible-label` for automation. Toast / status-region elements additionally take `accessible-live` ([#11646](https://github.com/slint-ui/slint/pull/11646)) so screen readers announce dynamic content.
3. **Smaller Rust source files** — `crates/tisvlay-gui/src/main.rs` is **2486 lines**, with `register_callbacks` alone at ~800 LoC. Carve into focused submodules so each future feature lands in a file with one concern. First-pass extractions identified below; deeper sub-splits (e.g. `register_callbacks` itself by pane) are tabled. The big `.slint` files (`tvs_pane.slint` 1180 lines, `pairing_wizard.slint` 1037 lines, `widgets.slint` 847 lines) are also long but each deserves its own focused PR; tabled to `docs/ideas.md`.
4. **Re-enable Slint's `accessibility` feature** — investigate the `accesskit_unix` / zbus-tokio conflict documented in `tisvlay-gui/Cargo.toml` and either resolve it or document the current upstream blockers more durably.

## Non-goals

- Pixel-snapshot / visual-regression tests. Slint upstream doesn't ship them in `i-slint-backend-testing`, ecosystem research consistently flags them as the most flake-prone test type, and the alternative (software-renderer + bundled fonts + tolerance threshold) is heavy maintenance for low signal. Tabled to `docs/ideas.md` if a real need surfaces.
- MCP-driven CI tests. `.claude/slint.md` already documents the reliability quirks (handle invalidation, mid-animation screenshots, sidebar-nav flake); MCP stays a dev-time introspection tool, not a CI gate. Tabled to `docs/ideas.md`.
- Modularizing the large `.slint` files (`tvs_pane.slint`, `pairing_wizard.slint`, `widgets.slint`). Each deserves its own focused PR; bundling here would inflate the diff and make the test-infra and accessibility work harder to review.
- Sub-splitting `main.rs::register_callbacks` (~800 LoC) by pane. The first-pass extractions below already shrink `main.rs` substantially; finer-grained breakup waits until the per-pane callback density actually warrants it.

## Test strategy — four layers

| Layer | Purpose | Crate | New deps |
|---|---|---|---|
| **A. Pure data-conversion** | View-model conversion (`state.rs`, `schema_form` helpers). No Slint runtime. Highest coverage, fastest tests. | inline `#[cfg(test)] mod tests` in `tisvlay-gui/src/` | none |
| **B. Slint integration smoke** | `AppWindow` instantiated, synthetic state pushed, element-tree assertions via the testing backend. One `#[test]` per integration-test file (Slint constraint). | `tisvlay-gui/tests/*.rs` | `i-slint-backend-testing` (dev-dep, pinned `=1.16.1` per upstream's "preliminary API, no semver" warning — [#11516](https://github.com/slint-ui/slint/pull/11516) merged 2026-04-27 already moved test helpers between modules; treat 1.17 as breaking) |
| **C. End-to-end via harness** | New `TestGui` fixture composes `TestSession` + `AppWindow` driven by the testing backend. One canonical e2e test: pair a mock TV, watch it appear in the master list. | `tisvlay-harness/src/gui.rs` (new), `tisvlay-harness/tests/gui_*.rs` | dev-dep on `tisvlay-gui` from `tisvlay-harness` |
| **D. CI hardening + docs** | `SLINT_EMIT_DEBUG_INFO=1` in `.cargo/config.toml [env]` and the GitHub Actions workflow env. `docs/testing.md` extended with a "GUI tests" section. | docs + `.cargo/config.toml` + `.github/workflows/` | none |

Reference precedent: [`slint/examples/todo/rust/lib.rs::press_add_adds_one_todo`](https://github.com/slint-ui/slint/blob/master/examples/todo/rust/lib.rs) is the only meaningful upstream Slint app test (~30 LoC, `init_no_event_loop` + `ElementQuery::match_id` + `set_accessible_value` + `invoke_accessible_default_action`). The wider Rust GUI ecosystem (`egui_kittest`, `iced_test` since Nov 2025, `kittest` framework-agnostic) converges on the same shape: headless event-loop + accessibility-tree queries + simulated input. Slint's testing backend matches that pattern minus pixel snapshots; ecosystem research treats the omission as conservative, not a gap.

## Refactor scope

Three reach-ins to make the GUI testable without a full event loop, plus the accessibility-label audit:

1. **`state.rs` IO/conversion split** — `fetch_state` mixes daemon/session bus calls with the JSON→`TvViewRaw` mapping. Split into `fetch_state` (IO; harness-driven) and `convert_snapshot(BusSnapshot) -> TvViewRaw` (pure; Layer-A unit-testable).
2. **`main.rs::push_snapshot` extraction** — extract the function that pushes a `StateSnapshot` into an `AppWindow` so tests can call it after constructing synthetic snapshots. Test-visibility via a `#[cfg(test)] pub(crate)` re-export; no lib-crate split needed.
3. **`main.rs` modularization** — first-pass extractions, each into a sibling `src/<name>.rs` module. Keeps `main.rs` to startup wiring (`Args`, `main()`, the `register_callbacks` orchestrator, the `tokio::spawn` calls into `state_watch`).

| New module | Existing in `main.rs` | Approx LoC |
|---|---|---|
| `src/about.rs` | `GUI_VERSION` / `REPO_URL` / `NOTICES_HTML*` constants, `installed_notices_path`, `open_notices_html`, `COMPONENT_CATALOGUE`, `drift_tone`, `probe_cli_version`, `make_about_components`, `build_diagnostic_blob`, `SupportedVendorsCache` | ~270 |
| `src/toast.rs` | `TOAST_DURATION_MS`, `TOAST_GEN`, `show_toast` | ~40 |
| `src/accent.rs` | `ashpd_color_to_slint`, `push_accent`, `watch_accent_color` | ~70 |
| `src/icon_theme.rs` | `detect_icon_theme`, `load_themed_icon` | ~80 |
| `src/wizard.rs` | `Reconcilers`, `WizardEntry`, `WizardEntries`, `publish_wizard_entries`, `reset_wizard`, `run_wizard_discovery`, `fetch_and_publish_schema`, `run_wizard_pair`, `run_pair_no_progress`, `publish_pair_outcome`, `mark_field_invalid` | ~340 |
| `src/edit_pane.rs` | `compute_edit_valid`, `mark_edit_field_invalid`, `publish_edit_error`, `parse_invalid_vendor_config`, `extract_invalid_vendor_config` | ~100 |
| `src/power_cycle.rs` | `run_power_on_with_progress`, `update_power_cycle_phase` | ~55 |
| `src/state_watch.rs` | `initial_fetch_and_watch`, `watch_state`, `refresh`, `refresh_about_components`, `push_snapshot`, `sync_vec_model`, `on_ui`, `push_tvs_error` | ~410 |

Target after extraction: `main.rs` ≈ 1100 lines (`Args` + `main()` + `register_callbacks`), the rest distributed across the new modules. `register_callbacks` itself stays in `main.rs` for this PR; sub-splitting it by pane is tabled.
4. **Accessibility-property audit** — every interactive element gets the right combination of `accessible-label` (translated, for screen readers) and `accessible-id` (literal, stable test selector). Toast + inline-error regions add `accessible-live`. Worst-case ~30 widgets across `main.slint`, `tvs_pane.slint`, `about_pane.slint`, `pairing_wizard.slint`, `profiles_pane.slint`, `session_pane.slint`, `settings_pane.slint`, `widgets.slint`. Where the visible label is a glyph (sidebar status dots, edit/save/cancel triplet, copy-icon button) `accessible-label` is the spelled-out action. `accessible-id` values follow a `<pane>-<element>` convention (e.g. `"about-diagnostics-button"`, `"tvs-row-{uuid}"`) so test selectors don't collide with translated UI strings.

## Slint `accessibility` feature investigation

The current Cargo.toml comment captures the symptom (`accesskit_unix` spawns a thread → `zbus` tokio backend panics with "no reactor running"). Upstream context worth surfacing:

- **The runtime conflict isn't filed upstream yet** — `gh search issues --repo slint-ui/slint "no reactor"` returns zero hits. If we want this fixed at the source we'd be the ones to file it, ideally with a minimal repro.
- **AccessKit on Slint is currently fragile by upstream's own admission** — [slint-ui/slint#11663](https://github.com/slint-ui/slint/pull/11663) (open, 2026-05-08) ports to winit 0.31 and **temporarily disables accesskit** because it requires upstream changes. Our "feature off" posture aligns with maintainers' own current stance.
- **A concrete pre-existing bug for our exact usage pattern** — [slint-ui/slint#10269](https://github.com/slint-ui/slint/issues/10269) (open since 2025-12-11) reports a panic in `accesskit_consumer` when `LineEdit`/`Slider` is clicked after a state mutation driven by `upgrade_in_event_loop`. `tisvlay-gui` runs that pattern continuously (Daemon-event → Slint property update → user clicks). Re-enabling the feature today would expose this independent of any zbus-runtime fix.
- **One regression class is already gone** — [slint-ui/slint#9864](https://github.com/slint-ui/slint/pull/9864) (merged 2025-10-28) stops the `accessibility` feature from force-enabling `winit/x11`, so Wayland-only NixOS builds finally work cleanly when we *do* turn it on.
- **Heavy perf cost on big ListViews remains open** — [slint-ui/slint#3867](https://github.com/slint-ui/slint/issues/3867) flags `i_slint_backend_winit::accesskit` as flamegraph-dominant on large lists. Reinforces "feature off in production until needed" as the right default even if we resolve the runtime issue.

For this sprint:

- **Reproduce + isolate** — flip the feature on, capture the exact stack trace under current zbus 5.x + accesskit. Confirm the panic still happens; record whether [#10269](https://github.com/slint-ui/slint/issues/10269) also reproduces in our pattern.
- **Triage paths**:
  - **Path A: configure zbus to share our tokio runtime** — the `tokio::runtime::Handle::try_current()` route, possibly wrapping `accesskit_unix`'s connection setup with `tokio::task::block_in_place` or by handing it a pre-built `zbus::Connection` from our runtime.
  - **Path B: switch zbus to async-io for accesskit only** — zbus has feature-flag selection between tokio and async-io. Whether two runtimes can coexist in one process is the open question.
  - **Path C: file an upstream issue against `accesskit_unix` and/or slint with a minimal repro** — even if A or B pan out, surfacing the symptom upstream helps the next user hitting it.
- **Definition of done** — at minimum, the comment block in `Cargo.toml` updates to reference the verified-as-of-this-sprint root cause + links to [#11663](https://github.com/slint-ui/slint/pull/11663) / [#10269](https://github.com/slint-ui/slint/issues/10269) / [#3867](https://github.com/slint-ui/slint/issues/3867). Best case: the feature re-enables with a workaround AND we file an upstream issue. The test infrastructure does **not** depend on this — Slint's testing backend reads `accessible-*` properties via the internal element tree, not via AT-SPI, so labels and ids work in tests regardless of the production feature state.

## Sequencing inside the PR

So the diff doesn't sprawl, internal commits land in this order:

1. **`refactor(gui): split state.rs IO from view-model conversion`** + Layer-A unit tests for the conversion path.
2. **`test(gui): cover schema_form helpers`** — pure refactor of test surface for `autofill_schema`, `validate_value`, `prefill_schema_from_config`, `compute_schema_valid`.
3. **`refactor(gui): carve main.rs into focused submodules`** — extract `about` / `toast` / `accent` / `icon_theme` / `wizard` / `edit_pane` / `power_cycle` / `state_watch` per the table above. Mechanical move + visibility adjustment, no behavior change.
4. **`feat(gui): add accessible-label across interactive widgets`** — `.slint`-only diff, gettext catalog refresh via `just i18n-extract`, easy to review.
5. **`feat(gui): wire i-slint-backend-testing for headless tests`** + Layer-B smoke tests (`AppWindow` boots, About-pane components render, Diagnostics button click round-trips).
6. **`feat(harness): add TestGui fixture`** — `tisvlay-harness/src/gui.rs`, the once-per-process init guard, and the canonical e2e test (`tisvlay-harness/tests/gui_pair_flow.rs`).
7. **`ci: set SLINT_EMIT_DEBUG_INFO=1; document GUI test path`** — `.cargo/config.toml [env]` + workflow env + `docs/testing.md` "GUI tests" section.
8. **`chore(gui): investigate accessibility feature; document findings`** — Cargo.toml comment refresh, optional re-enable if a workaround panned out.

The investigation in step 8 is fact-finding-bounded — if it lands at "no clean workaround, here's why," that's a legitimate result; the comment block + an `docs/ideas.md` entry capture the state. The other seven steps proceed independently.

## Out of scope (tabled to `docs/ideas.md`)

| Topic | Idea entry |
|---|---|
| Modularize `tvs_pane.slint` (1180 lines) | "Modularize tvs_pane.slint" |
| Modularize `pairing_wizard.slint` (1037 lines) | "Modularize pairing_wizard.slint" |
| Modularize `widgets.slint` (847 lines) | "Split widgets.slint into per-primitive files" |
| Pixel-snapshot / visual regression tests | "Pixel-snapshot tests for tisvlay-gui" |
| MCP-driven CI smoke tests | "MCP-driven GUI smoke tests" |

Each is a meaningful follow-up but doesn't belong on the critical path of "make GUI testable so the original M3.5 / M4 / M5 roadmap has a regression net to land in."
