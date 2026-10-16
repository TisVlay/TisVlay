# GUI-First Code Coverage Expansion

## Goal

Increase meaningful production-code coverage in staged, reviewable increments. Begin with deterministic GUI helper tests, then make callback registration testable and cover the large user-facing GUI gaps. Use a fresh report to choose follow-on areas such as CLI, session, or tray behavior.

The latest saved report showed approximately 64.4% line coverage (18,442 of 28,642 lines), while `cov_baseline_lines` is 52%. Treat that report as a pointer only: regenerate coverage on the intended checkout before setting targets or changing the ratchet. Coverage is a signal, not the goal; tests should assert behavior that matters.

## Scope and Sequence

### 1. Capture a current baseline

Run `just cov` and record total and per-file line coverage, especially for the GUI app, CLI, session, tray, and runtime. Confirm the denominator and test result before comparing changes. Do not change the ratchet just to match the saved report.

### 2. Add deterministic GUI unit tests

Use existing private unit-test modules and synthetic Slint models; avoid introducing a daemon fixture for these cases.

- In `crates/tisvlay-gui-app/src/wizard.rs`, test `mark_field_invalid` for a matching field (invalid state, supplied reason, and recomputed validity) and an unknown field (returns false for generic-error handling).
- In `crates/tisvlay-gui-app/src/edit_pane.rs`, test `mark_edit_field_invalid` for a matching and absent field using a synthetic `PairingFieldRow`/`VecModel`, and verify validity updates.
- Cover `reset_wizard` cancellation and generation behavior only if it remains self-contained in the same slice.

Keep the helpers private and follow the existing unit-test conventions.

### 3. Establish a testable callback boundary

In `crates/tisvlay-gui-app/src/lib.rs`, separate app setup and callback registration from OS/client initialization and the blocking Slint event loop. Before building a callback suite, prove one callback can be invoked deterministically and that any `on_ui` update is observable with the selected testing backend. Keep test seams crate-private; do not expand the public SDK surface for tests.

Reuse the existing `crates/tisvlay-gui-app/tests/it/` binary and add modules there rather than adding another integration-test binary. Do not assume a `TestGui` fixture or `MockHandle::expect_volume` exists.

### 4. Cover representative GUI workflows

Once the callback boundary is proven, add focused workflow tests for the highest-value user paths:

- Pairing success and vendor-field error routing.
- Edit-form save and configuration update.
- Daemon appear/disappear state-watch behavior.
- Common volume, mute, backlight, and screen-toggle actions with observable UI and daemon effects.

Use mock state publication for volume-like commands and existing call counters for screen operations. Add mock controls only when an important behavior cannot otherwise be observed and the controls have value beyond one assertion. Use a socket-scoped daemon/Zenoh fixture only if the callback tests need a real service stack; follow the existing pattern in `crates/tisvlay-daemon/tests/it/wire.rs`.

### 5. Select further work from the new report

Rerun coverage and rank the next slice by uncovered production lines and behavioral risk. CLI dispatch/error paths in `crates/tisvlay-cli/src/main.rs` are an early candidate; choose specific session or tray logic only after inspecting the fresh report. Prefer pure or mockable decision logic over process-entrypoint glue. Defer portal-, compositor-, filesystem-, and environment-dependent modules unless their regression risk justifies the setup.

### 6. Raise the ratchet and retire this plan

After tests pass and the measured denominator is stable, raise `cov_baseline_lines` to the verified value; never lower it. When the effort is complete, move durable testing guidance into `docs/testing.md`, remove this plan and its active-plan entry, and sweep inbound references as required by `docs/plans/README.md`.

## Validation

- Helper-test phase: `cargo nextest run --workspace --all-features -E 'package(tisvlay-gui-app)'`.
- Callback/workflow phase: `cargo nextest run --workspace --all-features -E 'binary_id(tisvlay-gui-app::it)'`.
- At each substantial phase, run `just cov` and inspect both affected files and the total. Use `just cov-html` when line attribution is unclear.
- Before merging coverage-affecting changes, run `just test-ci` to validate the full instrumented pipeline and ratchet. Change the ratchet only after tests pass and the intended production-code denominator is confirmed.
- For this plan document, run `just typos`. Run `just docs-links` if changes also touch the mdBook.

## Decisions and Risks

- Sequence the work as helper tests, callback-test foundation, then a measured broader sweep. Keep each substantial phase independently reviewable.
- The main technical unknown is deterministic Slint event-loop pumping, especially for asynchronous `on_ui` work. Prove that boundary with one callback before expanding the suite.
- Use current source and mock APIs as the authority. Earlier GUI coverage notes refer to stale `TestGui`/`tisvlay-harness` infrastructure and old module figures.
- Keep unrelated packaging and documentation edits out of scope and preserve existing user changes.
- If callback tests require heavy linking or unreliable desktop services, keep those dependencies out of the helper-test phase and narrow the test setup before continuing.
