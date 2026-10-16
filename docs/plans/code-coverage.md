# Behavior-Led Code Coverage Expansion

## Goal

Increase meaningful production-code coverage in staged, reviewable increments. Start with deterministic GUI error-routing tests, then use a fresh report and the architecture's test boundaries to select the next behavior. Keep UI-state tests in the GUI, RPC and wire-contract tests in SDK/service suites, and avoid duplicating behavior already covered at those layers.

The latest saved report showed approximately 64.4% line coverage (18,442 of 28,642 lines), while `cov_baseline_lines` is 52%. Treat that report as a pointer only: regenerate coverage on the intended checkout before setting targets or changing the ratchet. Coverage is a signal, not the goal; tests should assert behavior that matters.

## Scope and Sequence

### 1. Capture a current baseline

Run `just cov` and record total and per-file line coverage, especially for the GUI app, CLI, session, tray, and runtime. Confirm the denominator and test result before comparing changes. Do not change the ratchet just to match the saved report.

### 2. Add deterministic GUI error-routing tests

Use existing private unit-test modules and synthetic Slint models; avoid introducing a daemon fixture for these cases. These test GUI-specific mapping from an invalid-field error to inline form state, not the wire error contract itself.

- In `crates/tisvlay-gui-app/src/wizard.rs`, test `mark_field_invalid` for a matching field (invalid state, supplied reason, and recomputed validity) and an unknown field (returns false for generic-error handling).
- In `crates/tisvlay-gui-app/src/edit_pane.rs`, test `mark_edit_field_invalid` for a matching and absent field using a synthetic `PairingFieldRow`/`VecModel`, and verify validity updates.
- Cover `reset_wizard` cancellation and generation behavior only if it remains self-contained in the same slice.

Keep the helpers private and follow the existing unit-test conventions. Do not make callback registration or `run()` testable in this first slice.

### 3. Re-rank from the fresh report and existing test inventory

After the helper tests, rerun coverage and inspect uncovered production lines alongside the existing tests. Rank candidates by behavioral risk and likely coverage gain, not percentage alone. The saved report is not enough to justify broad GUI callback work or a workspace target.

Use the repository's established test layer for the behavior:

- **GUI-local behavior** (form validity/error routing, model projections, banners, capability-gated rendering, synchronous widget state) belongs in `tisvlay-gui-app` unit tests or its existing `tests/it/` binary. Those tests use synthetic models and `init_no_event_loop()`; they do not boot a daemon or assert asynchronous worker updates.
- **RPC behavior and wire-visible state** belong in the SDK and service integration suites. The daemon wire suite already covers volume, input, backlight, screen actions, configuration validation/update, pairing progress and cancellation. SDK tests already cover pairing, volume round-trip, mirror hydration, removal, late daemon appearance, and watcher/version events. Extend those suites only for a missing contract; do not repeat them as GUI workflows just to increase GUI coverage.
- **GUI callback glue** is a candidate only if the fresh report and behavior review identify a GUI-specific gap not covered by those suites. First prove a narrow, deterministic test of GUI-to-SDK dispatch. The current GUI backend has no event loop, and callback work is asynchronous; do not assert worker-posted UI changes in a no-event-loop test. Extract a crate-private boundary only as needed, and avoid refactoring all of `run()` as a prerequisite.

For state watching, target the GUI's distinct projection/clearing behavior on service appear/disappear, not daemon availability or SDK watcher delivery already tested elsewhere. For CLI, prefer meaningful command parsing or error-mapping decisions over process-entrypoint glue. For session and tray, inspect their current test inventories before adding integration coverage.

- GUI-only projection when a service appears or disappears, if that behavior is not already asserted by the existing UI tests.

### 4. Select measured follow-on work

Choose the next one or two slices from the fresh report and existing test inventory. Candidates include GUI-only state projection/error display, untested CLI decisions, or specific session/tray behavior. Prefer pure or mockable decision logic; use a wire test only when the behavior is part of the service/SDK contract. Defer portal-, compositor-, filesystem-, and environment-dependent modules unless their regression risk justifies the setup. Any new GUI integration tests belong as modules in the existing test binary, not a second `tests/*.rs` binary.

### 5. Raise the ratchet and retire this plan

After tests pass and the measured denominator is stable, raise `cov_baseline_lines` to the verified value; never lower it. When the effort is complete, move durable testing guidance into `docs/testing.md`, remove this plan and its active-plan entry, and sweep inbound references as required by `docs/plans/README.md`.

## Validation

- Helper-test phase: `cargo nextest run --workspace --all-features -E 'package(tisvlay-gui-app)'`.
- If GUI integration modules change, run `cargo nextest run --workspace --all-features -E 'binary_id(tisvlay-gui-app::it)'`.
- At each substantial phase, run `just cov` and inspect both affected files and the total. Use `just cov-html` when line attribution is unclear.
- Before merging coverage-affecting changes, run `just test-ci` to validate the full instrumented pipeline and ratchet. Change the ratchet only after tests pass and the intended production-code denominator is confirmed.
- For this plan document, run `just typos`. Run `just docs-links` if changes also touch the mdBook.

## Decisions and Risks

- Sequence the work as focused GUI error-routing tests, a coverage/test-inventory review, then measured behavior slices at the right UI, SDK, or service test layer. Keep each substantial phase independently reviewable.
- Follow `docs/testing.md`: the current GUI test backend is for rendering/UI state without an event loop; service integration tests boot real `serve()` stacks and assert the wire contract. The one-integration-binary-per-crate rule still applies.
- Follow `docs/architecture.md`: GUI is a client of the SDK over two sockets; SDK and wire/service tests own RPC contracts and service state behavior. Coverage work should not conflate GUI presentation with service behavior.
- Use current source and mock APIs as the authority. Earlier GUI coverage notes refer to stale `TestGui`/`tisvlay-harness` infrastructure and old module figures.
- Keep unrelated packaging and documentation edits out of scope and preserve existing user changes.
- Callback tests are conditional. If a GUI-specific gap needs them, first prove deterministic event-loop handling for one callback and asynchronous `on_ui` update; otherwise test narrower decision/projection logic without refactoring startup.
- Avoid duplication: daemon wire tests already cover core TV command/config/pairing contracts, and SDK client tests cover watcher/client behavior. GUI tests should add coverage for GUI-only state and rendering.
- The helper slice is intentionally small and may yield only a modest workspace percentage increase; use it to improve failure-path confidence, then let the measured report guide higher-return work.
- Keep tests hermetic. If a candidate needs D-Bus, compositor, filesystem, or other host state, introduce a documented seam/fake consistent with `docs/testing.md`, or defer it.
