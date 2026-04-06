# Plan: Testing & Measurement Framework

## Goal

Establish an extensible testing and measurement framework for the tisvlay workspace. The framework should:

1. Make it cheap to add new tests (low friction → more tests get written)
2. Catch regressions before they hit production
3. Measure code coverage and treat it as a property to monitor over time
4. Support LLM-assisted development (clear conventions, fast feedback, structured outputs)
5. Run in CI on every PR with results visible inline
6. Lay the groundwork for a future mock backend

## Current state

- **Workspace**: 8 crates (`tisvlay-core`, `tisvlay-lg`, `tisvlay-backends`, `tisvlayd`, `tisvlay-cli`, `tisvlay-session`, `tisvlay-tray`, `tisvlay-core` D-Bus integration test)
- **Test count**: ~10 unit tests scattered across `config.rs`, `wol.rs`, `pairing.rs`, `capabilities.rs`, `error.rs`, `dbus_paths.rs`, `discovery.rs`, `lib.rs`, `state.rs`, `netlink.rs`. One integration test (`tisvlay-core/tests/dbus_client.rs`) using a mock zbus interface.
- **Test runner**: `cargo test --workspace --all-features` (built-in)
- **CI**: `.github/workflows/ci.yml` runs prek (file checks + fmt) then build/clippy/test directly. No coverage, no test reports, no nextest.
- **Local pre-commit**: `prek.toml` with the same checks.
- **No coverage measurement**, no benchmark infrastructure, no mock backend, no property tests.

## Design principles

These are the choices that shape the framework. Each is justified inline.

### 1. Coverage as a property to monitor, not a gate

Set up `cargo-llvm-cov` to generate reports locally and in CI. Upload to Codecov (free for OSS) for trend tracking and PR diff annotations. **Do not** set a hard minimum threshold initially — the current coverage is unknown and an arbitrary minimum (e.g., 80%) would either be trivially met or block all PRs. Instead:

1. Measure baseline coverage
2. Add a "coverage must not decrease" CI check (`--fail-under-lines` set to current baseline, bumped on PRs that improve coverage)
3. Track trend over time via Codecov

This is the "ratchet" approach from the primer. It pressures new code to come with tests without blocking the existing untested surface.

### 2. Layered test types matching their costs

| Type | Where | When | Tool |
|------|-------|------|------|
| Unit tests | `#[cfg(test)] mod tests` inside source | every change | built-in |
| Integration tests | `crates/*/tests/` | every change | built-in |
| Doc tests | `///` examples in `pub` items | every change | built-in |
| Snapshot tests | `crates/*/tests/snapshots/` | every change | `insta` |
| Property tests | inside unit/integration tests | every change | `proptest` (existing ecosystem fit) |
| Mock backend tests | `crates/tisvlay-mock/` (future) | every change | custom |
| Fuzzing | `fuzz/` (future) | scheduled | `cargo-fuzz` |

The first three are free (built-in). The next three are the immediate additions. The last is future work.

### 3. Single test runner: `cargo-nextest`

Switch from `cargo test` to `cargo nextest`. Reasons:
- Process-per-test isolation matters because the daemon code uses global state (D-Bus, netlink, file paths)
- JUnit XML output for inline PR test results in GitHub Actions
- Faster than `cargo test` on cold runs
- Configurable per-profile (`default` for local, `ci` for CI)

Keep `cargo test` working as a fallback (nextest doesn't support doc tests directly — those still need `cargo test --doc`).

### 4. Keep it transport-agnostic and vendor-agnostic

The same architecture lessons from the daemon apply: test layers should not know about specific vendors. The mock backend (future) implements `TvBackend`/`TvConnector`/`TvConnection` so any test that exercises the daemon can swap LG for the mock without changes. Tests of vendor-specific code stay in the vendor crate.

### 5. LLM-friendly conventions

- **Predictable test layout**: unit tests in `mod tests`, integration tests in `tests/`, snapshot files in `tests/snapshots/`. No magic.
- **Snapshot tests** (insta) for anything with structured output (config parsing, D-Bus message format, CLI output). LLMs can generate these confidently because the assertion is "the output matched the recorded version."
- **Fast feedback**: full test suite in under 30s; coverage report in under 60s. Slow tests get marked `#[ignore]` and run on demand.
- **Test names describe behavior**, not method names: `nm_disconnect_does_not_affect_other_nics` not `test_disconnect`.
- **`cargo nextest --profile ci` outputs JUnit XML** which LLMs can parse from CI logs to identify exactly which test broke.

## Implementation plan

### Phase 1: Coverage measurement (immediate)

**Goal**: see where the gaps are.

1. Add `cargo-llvm-cov` install instructions to `.claude/CLAUDE.md`
2. Add `coverage` task to `.vscode/tasks.json` for local HTML reports
3. Add `make coverage` script (or similar) for the standard ratchet command
4. Generate baseline coverage report and commit it as the initial threshold
5. Add Codecov upload step to CI (optional — defer if Codecov account not desired)

**No code changes required** beyond CI/scripts. This phase just establishes measurement.

**Files**:
- `.github/workflows/coverage.yml` (new) — separate workflow for coverage
- `.claude/CLAUDE.md` — add "Running coverage" section
- `scripts/coverage.sh` (new, optional) — local coverage helper

### Phase 2: Test runner upgrade (`cargo-nextest`)

**Goal**: better test isolation and CI reporting.

1. Add `.config/nextest.toml` with `default` and `ci` profiles
2. Update `prek.toml` to use `cargo nextest run` for the test hook
3. Update `.github/workflows/ci.yml`:
   - Install `cargo-nextest` via `taiki-e/install-action`
   - Run `cargo nextest run --profile ci` instead of `cargo test`
   - Add `dorny/test-reporter` to display JUnit results inline on PRs
4. Keep `cargo test --doc` as a separate step for doc tests (nextest doesn't run them)
5. Document the workflow in `.claude/CLAUDE.md`

**No source code changes** — existing `#[test]` and `#[tokio::test]` functions work unchanged.

**Files**:
- `.config/nextest.toml` (new)
- `.github/workflows/ci.yml` (modified)
- `prek.toml` (modified)
- `.claude/CLAUDE.md` (modified)

### Phase 3: Snapshot test infrastructure (`insta`)

**Goal**: cheap regression tests for structured output.

1. Add `insta` to workspace dev-dependencies
2. Add a single example snapshot test to demonstrate the pattern (e.g., `TvInfo` JSON serialization in the CLI crate)
3. Document the workflow: `cargo insta test`, `cargo insta review`, `cargo insta accept`
4. Add `cargo-insta` to the install instructions

**Files**:
- `Cargo.toml` (workspace) — add `insta` to workspace deps
- one example snapshot test demonstrating the pattern
- `.claude/CLAUDE.md` — add snapshot workflow section

### Phase 4: Property test infrastructure (`proptest`)

**Goal**: edge-case discovery for input-handling code.

1. Add `proptest` to workspace dev-dependencies
2. Identify 1-2 functions that benefit from property testing as initial demonstrations:
   - `dbus_paths::tv_path` sanitization (any input → valid D-Bus path)
   - `wol::MacAddress` parsing (roundtrip stability)
3. Add a `tests/proptest-regressions/` ignore note if generated
4. Document the pattern

**Files**:
- `Cargo.toml` (workspace) — add `proptest`
- 1-2 example proptest in existing crates
- `.claude/CLAUDE.md` — add property test section

### Phase 5: Mock backend skeleton (foundation, no tests yet)

**Goal**: lay the groundwork for future feature development without committing to the full mock yet.

1. Create `crates/tisvlay-mock/` crate
2. Implement minimal `TvBackend`, `TvConnector`, `TvConnection` returning sensible defaults (power state, volume, etc. as in-memory state)
3. Make it discoverable via `tisvlay-backends` (gated behind a `mock` feature)
4. Add one integration test that exercises the daemon end-to-end with the mock backend
5. Document its purpose and how to extend it

This is the foundation for testing high-level features (UX/UI, GUI client) without requiring a real TV. **Defer** until phases 1-4 prove the framework is working.

**Files**:
- `crates/tisvlay-mock/` (new crate)
- `crates/tisvlay-backends/Cargo.toml` (modified) — add `mock` feature
- One integration test in `crates/tisvlayd/tests/`

### Phase 6: CI integration polish

**Goal**: tighten the feedback loop on PRs.

1. Coverage workflow uploads to Codecov on PRs (with diff annotations)
2. Test workflow uploads JUnit XML for inline PR results
3. Coverage check uses `--fail-under-lines` set to baseline
4. Optional: separate "fast tier" and "thorough tier" workflows per primer recommendation

**Files**: `.github/workflows/*.yml`

## Out of scope (for now)

These belong in a future plan once the framework is established:

- **Mutation testing** (`cargo-mutants`) — too slow and noisy until the basic test suite has good coverage
- **Fuzzing** (`cargo-fuzz`) — requires clear "input parsing" entry points; defer until we have something worth fuzzing
- **Benchmark infrastructure** (`criterion`, `bencher`) — performance is not a current concern for tisvlay
- **Mutation testing alongside coverage** — wait for the test suite to be more complete first
- **`cargo-deny` / `cargo-audit`** — separate concern from testing; tracked elsewhere

## Open questions

1. **Codecov vs self-hosted vs no service?** Codecov is free for OSS and gives PR annotations. The tradeoff is depending on a third-party service. Defer the decision: run coverage in CI without uploading first, then decide.
2. **Where does the mock backend live?** New crate `tisvlay-mock` vs feature flag on an existing crate. New crate is cleaner for the dependency graph but adds workspace overhead. The plan assumes new crate.
3. **Should we adopt `rstest` for parameterized tests?** Useful for table-driven tests but adds a dependency. Defer until we have a concrete use case.
4. **What's the coverage baseline target?** Cannot know until phase 1 completes. The ratchet approach handles this — start where we are.

## Implementation order

Phases are ordered to give value early and minimize risk:

1. **Phase 1** (coverage) is pure infrastructure with no code changes — lowest risk, highest information value
2. **Phase 2** (nextest) is a one-time switch with immediate CI improvements
3. **Phase 3** (insta) and **Phase 4** (proptest) can happen in parallel — both are additive
4. **Phase 5** (mock backend) requires phases 1-4 to validate the framework is solid
5. **Phase 6** (CI polish) is iterative throughout

Do phase 1 first, evaluate coverage results, and decide whether to proceed with phases 2-4 in parallel or sequentially based on what the coverage report reveals.
